import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.KeyFactory;
import java.security.MessageDigest;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.Signature;
import java.security.interfaces.ECPublicKey;
import java.security.spec.PKCS8EncodedKeySpec;
import java.security.spec.X509EncodedKeySpec;
import java.time.LocalDateTime;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Properties;
import java.util.TreeMap;
import java.util.regex.Pattern;
import java.util.stream.Stream;
import java.util.zip.CRC32;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;
import java.util.zip.ZipOutputStream;

/**
 * DroidBridge release helper (JDK 17 source-file mode, standard library only).
 *
 * <p>Commands:
 * <pre>
 *   --i0-probe
 *   check-config                                   fail unless all six release values are provisioned and valid
 *   module-zip STAGING_DIR OUT_ZIP                 deterministic Magisk ZIP
 *   manifest VERSION PUBLISHED_AT DIST_DIR         canonical DIST_DIR/release.json from the final artifacts
 *   sign RELEASE_JSON PKCS8_PEM OUT_SIG            SHA256withECDSA over the exact manifest bytes
 *   sums DIST_DIR VERSION                          DIST_DIR/SHA256SUMS.txt over the five fixed files
 *   apk-unsigned APK                               fail if the APK carries any signature
 *   verify-release DIST_DIR VERSION unsigned|signed full manifest/signature/checksum verification
 * </pre>
 * Private keys are only read from the operator-supplied path and are never printed.
 */
public final class ReleaseTool {
    private static final List<String> CONFIG_KEYS = List.of(
            "github_owner",
            "github_repo",
            "manifest_url",
            "apk_signer_sha256",
            "release_key_id",
            "release_public_key_base64"
    );
    private static final String UNCONFIGURED = "UNCONFIGURED";
    private static final Path CONFIG = Path.of("release-config.properties");
    private static final Path TEST_PUBLIC_KEY = Path.of("tools/fixtures/release/manifest-test-public-key.der");

    private static final Pattern SEMVER = Pattern.compile("(0|[1-9]\\d{0,2})\\.(0|[1-9]\\d{0,2})\\.(0|[1-9]\\d{0,2})");
    private static final Pattern HEX64 = Pattern.compile("[0-9a-f]{64}");
    private static final Pattern HEX40 = Pattern.compile("[0-9a-f]{40}");
    private static final Pattern REPO_PART = Pattern.compile("[A-Za-z0-9][A-Za-z0-9._-]{0,99}");
    private static final Pattern KEY_ID = Pattern.compile("[a-z0-9][a-z0-9-]{0,63}");
    private static final Pattern PUBLISHED_AT = Pattern.compile("\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}Z");
    private static final long MAX_ARTIFACT_BYTES = 536_870_912L;

    /** S-STACK-001..011 tool identities recorded in every manifest's provenance. */
    private static final Map<String, String> TOOLS = orderedMap(
            "jdk", "17",
            "agp", "9.4.0",
            "gradle", "9.6.0",
            "kotlin", "2.4.10",
            "ndk", "29.0.14206865",
            "cmake", "3.31.6",
            "ninja", "1.12.1",
            "winflexbison", "2.5.25",
            "rust", "1.98.0",
            "cargo_ndk", "4.1.2"
    );
    private static final String LIBPCAP_SOURCE_SHA256 = "872dd11337fe1ab02ad9d4fee047c9da244d695c6ddf34e2ebb733efd4ed8aa9";
    private static final List<String> MANIFEST_KEYS = List.of(
            "schema_version", "channel", "version", "version_code", "published_at", "min_android_sdk",
            "protocol_version", "store_schema_version", "provenance", "artifacts", "release_notes_url");
    private static final List<String> PROVENANCE_KEYS = List.of(
            "source_revision", "jdk", "agp", "gradle", "kotlin", "ndk", "cmake", "ninja", "winflexbison", "rust",
            "cargo_ndk", "libpcap_source_sha256", "libpcap_patch_sha256", "gradle_lock_sha256",
            "gradle_verification_sha256", "cargo_lock_sha256", "apk_signer_sha256", "release_key_id");
    private static final List<String> ARTIFACT_KEYS = List.of("name", "url", "size", "sha256");

    private ReleaseTool() {}

    public static void main(String[] args) throws Exception {
        if (Runtime.version().feature() != 17) {
            throw new IllegalStateException("ReleaseTool requires JDK 17, found " + Runtime.version());
        }
        String command = args.length == 0 ? "" : args[0];
        switch (command) {
            case "--i0-probe" -> {
                expectArgs(args, 1);
                loadConfig();
                System.out.println("ReleaseTool I0 probe OK: JDK17 source-file mode and canonical release configuration are available.");
            }
            case "check-config" -> {
                expectArgs(args, 1);
                ReleaseConfig.provisioned(loadConfig());
                System.out.println("release configuration provisioned");
            }
            case "module-zip" -> {
                expectArgs(args, 3);
                moduleZip(Path.of(args[1]), Path.of(args[2]));
            }
            case "manifest" -> {
                expectArgs(args, 4);
                writeManifest(args[1], args[2], Path.of(args[3]));
            }
            case "sign" -> {
                expectArgs(args, 4);
                sign(Path.of(args[1]), Path.of(args[2]), Path.of(args[3]));
            }
            case "sums" -> {
                expectArgs(args, 3);
                writeSums(Path.of(args[1]), args[2]);
            }
            case "apk-unsigned" -> {
                expectArgs(args, 2);
                requireUnsignedApk(Path.of(args[1]));
                System.out.println("APK carries no signature");
            }
            case "verify-release" -> {
                expectArgs(args, 4);
                verifyRelease(Path.of(args[1]), args[2], Mode.parse(args[3]));
            }
            default -> {
                System.err.println("usage: see ReleaseTool.java header");
                System.exit(2);
            }
        }
    }

    // ---- configuration --------------------------------------------------------------------------

    private static Properties loadConfig() throws IOException {
        if (!Files.isRegularFile(CONFIG)) throw new IllegalStateException("Missing release-config.properties");
        List<String> lines = Files.readAllLines(CONFIG, StandardCharsets.UTF_8);
        if (lines.size() != CONFIG_KEYS.size()) {
            throw new IllegalStateException("release-config.properties must contain exactly six entries");
        }
        for (int i = 0; i < CONFIG_KEYS.size(); i++) {
            String expectedPrefix = CONFIG_KEYS.get(i) + "=";
            if (!lines.get(i).startsWith(expectedPrefix)) {
                throw new IllegalStateException("release-config.properties key/order mismatch at line " + (i + 1));
            }
            if (lines.get(i).length() == expectedPrefix.length()) {
                throw new IllegalStateException("Empty release configuration value for " + CONFIG_KEYS.get(i));
            }
        }
        Properties properties = new Properties();
        try (InputStream input = Files.newInputStream(CONFIG)) {
            properties.load(input);
        }
        if (properties.size() != CONFIG_KEYS.size()) {
            throw new IllegalStateException("release-config.properties contains unknown or duplicate effective keys");
        }
        return properties;
    }

    private record ReleaseConfig(String owner, String repo, String manifestUrl, String apkSignerSha256,
                                 String releaseKeyId, byte[] publicKey) {
        static ReleaseConfig provisioned(Properties properties) throws Exception {
            for (String key : CONFIG_KEYS) {
                if (UNCONFIGURED.equals(properties.getProperty(key))) {
                    throw new IllegalStateException("release configuration value is UNCONFIGURED: " + key);
                }
            }
            String owner = matching(properties, "github_owner", REPO_PART);
            String repo = matching(properties, "github_repo", REPO_PART);
            String manifestUrl = properties.getProperty("manifest_url");
            String expectedUrl = "https://github.com/" + owner + "/" + repo + "/releases/latest/download/release.json";
            if (!expectedUrl.equals(manifestUrl)) {
                throw new IllegalStateException("manifest_url must be " + expectedUrl);
            }
            String signer = matching(properties, "apk_signer_sha256", HEX64);
            String keyId = matching(properties, "release_key_id", KEY_ID);
            byte[] publicKey = Base64.getDecoder().decode(matching(properties, "release_public_key_base64",
                    Pattern.compile("[A-Za-z0-9+/]+={0,2}")));
            requireP256(publicKey);
            return new ReleaseConfig(owner, repo, manifestUrl, signer, keyId, publicKey);
        }

        private static String matching(Properties properties, String key, Pattern pattern) {
            String value = properties.getProperty(key);
            if (!pattern.matcher(value).matches()) throw new IllegalStateException("invalid release configuration value: " + key);
            return value;
        }

        String releaseBase(String version) {
            return "https://github.com/" + owner + "/" + repo + "/releases/download/v" + version + "/";
        }

        String notesUrl(String version) {
            return "https://github.com/" + owner + "/" + repo + "/releases/tag/v" + version;
        }
    }

    // ---- artifact names/versions ----------------------------------------------------------------

    private static long versionCode(String version) {
        var matcher = SEMVER.matcher(version);
        if (!matcher.matches()) throw new IllegalStateException("version must be MAJOR.MINOR.PATCH with components 0..999");
        return Long.parseLong(matcher.group(1)) * 1_000_000L + Long.parseLong(matcher.group(2)) * 1_000L
                + Long.parseLong(matcher.group(3));
    }

    private static String apkName(String version) {
        return "droidbridge-" + version + "-arm64-v8a.apk";
    }

    private static String moduleName(String version) {
        return "droidbridge-magisk-" + version + ".zip";
    }

    private static List<String> distributedFiles(String version) {
        List<String> names = new ArrayList<>(List.of(apkName(version), moduleName(version), "release.json",
                "release.json.sig", "THIRD_PARTY_NOTICES.txt"));
        names.sort(String::compareTo);
        return names;
    }

    // ---- deterministic module ZIP ---------------------------------------------------------------

    private static void moduleZip(Path staging, Path out) throws IOException {
        TreeMap<String, Path> entries = new TreeMap<>();
        try (Stream<Path> walk = Files.walk(staging)) {
            for (Path file : (Iterable<Path>) walk.filter(Files::isRegularFile)::iterator) {
                String name = staging.relativize(file).toString().replace('\\', '/');
                if (name.startsWith("/") || name.contains("../") || name.equals("..")) {
                    throw new IllegalStateException("unsafe module entry: " + name);
                }
                if (entries.put(name, file) != null) throw new IllegalStateException("duplicate module entry: " + name);
            }
        }
        if (!entries.containsKey("module.prop")) throw new IllegalStateException("staging has no module.prop");
        Path temp = Files.createTempFile(out.toAbsolutePath().getParent(), ".module", ".zip");
        try (ZipOutputStream zip = new ZipOutputStream(Files.newOutputStream(temp), StandardCharsets.UTF_8)) {
            zip.setComment(null);
            for (var entry : entries.entrySet()) {
                byte[] bytes = Files.readAllBytes(entry.getValue());
                if (isScript(entry.getKey()) && indexOf(bytes, (byte) '\r') >= 0) {
                    throw new IllegalStateException("script must use LF line endings: " + entry.getKey());
                }
                ZipEntry zipEntry = new ZipEntry(entry.getKey());
                zipEntry.setTimeLocal(LocalDateTime.of(1980, 1, 1, 0, 0, 0));
                if (isStored(entry.getKey())) {
                    CRC32 crc = new CRC32();
                    crc.update(bytes);
                    zipEntry.setMethod(ZipEntry.STORED);
                    zipEntry.setSize(bytes.length);
                    zipEntry.setCompressedSize(bytes.length);
                    zipEntry.setCrc(crc.getValue());
                } else {
                    zipEntry.setMethod(ZipEntry.DEFLATED);
                }
                zip.putNextEntry(zipEntry);
                zip.write(bytes);
                zip.closeEntry();
            }
        }
        Files.move(temp, out, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
        System.out.println(sha256Hex(Files.readAllBytes(out)) + "  " + out.getFileName());
    }

    private static boolean isScript(String name) {
        return name.endsWith(".sh") || name.equals("module.prop") || name.startsWith("META-INF/");
    }

    /** Native executables and dex jars are already dense; text and notices are deflated. */
    private static boolean isStored(String name) {
        return name.startsWith("bin/") || name.endsWith(".jar");
    }

    // ---- manifest -------------------------------------------------------------------------------

    private static void writeManifest(String version, String publishedAt, Path dist) throws Exception {
        ReleaseConfig config = ReleaseConfig.provisioned(loadConfig());
        long code = versionCode(version);
        if (!PUBLISHED_AT.matcher(publishedAt).matches()) throw new IllegalStateException("published_at must be YYYY-MM-DDTHH:MM:SSZ");
        String revision = git("rev-parse", "HEAD").trim();
        if (!HEX40.matcher(revision).matches()) throw new IllegalStateException("unexpected git revision: " + revision);
        if (!git("status", "--porcelain", "--untracked-files=no").isBlank()) {
            throw new IllegalStateException("working tree has tracked modifications; release manifests name a clean revision");
        }

        Map<String, Object> provenance = new LinkedHashMap<>();
        provenance.put("source_revision", revision);
        provenance.putAll(TOOLS);
        provenance.put("libpcap_source_sha256", LIBPCAP_SOURCE_SHA256);
        provenance.put("libpcap_patch_sha256", sha256Hex(Files.readAllBytes(Path.of("tools/patches/libpcap-1.10.6-host-null-device.patch"))));
        provenance.put("gradle_lock_sha256", gradleLockSha256());
        provenance.put("gradle_verification_sha256", sha256Hex(Files.readAllBytes(Path.of("gradle/verification-metadata.xml"))));
        provenance.put("cargo_lock_sha256", sha256Hex(Files.readAllBytes(Path.of("rust/Cargo.lock"))));
        provenance.put("apk_signer_sha256", config.apkSignerSha256());
        provenance.put("release_key_id", config.releaseKeyId());

        Map<String, Object> artifacts = new LinkedHashMap<>();
        artifacts.put("apk", artifact(dist, apkName(version), config.releaseBase(version)));
        artifacts.put("magisk", artifact(dist, moduleName(version), config.releaseBase(version)));

        Map<String, Object> manifest = new LinkedHashMap<>();
        manifest.put("schema_version", 1L);
        manifest.put("channel", "stable");
        manifest.put("version", version);
        manifest.put("version_code", code);
        manifest.put("published_at", publishedAt);
        manifest.put("min_android_sdk", 33L);
        manifest.put("protocol_version", 1L);
        manifest.put("store_schema_version", 1L);
        manifest.put("provenance", provenance);
        manifest.put("artifacts", artifacts);
        manifest.put("release_notes_url", config.notesUrl(version));

        StringBuilder json = new StringBuilder();
        writeJson(json, manifest);
        json.append('\n');
        Files.writeString(dist.resolve("release.json"), json, StandardCharsets.UTF_8);
        System.out.println("wrote " + dist.resolve("release.json"));
    }

    private static Map<String, Object> artifact(Path dist, String name, String base) throws IOException {
        Path file = dist.resolve(name);
        long size = Files.size(file);
        if (size < 1 || size > MAX_ARTIFACT_BYTES) throw new IllegalStateException("artifact size out of range: " + name);
        Map<String, Object> artifact = new LinkedHashMap<>();
        artifact.put("name", name);
        artifact.put("url", base + name);
        artifact.put("size", size);
        artifact.put("sha256", sha256Hex(Files.readAllBytes(file)));
        return artifact;
    }

    /** Sorted committed lock files as UTF8(path) 0x00 bytes 0x00, hashed once. */
    private static String gradleLockSha256() throws Exception {
        List<String> paths = new ArrayList<>();
        for (String path : git("ls-files", "-z", "--", "*gradle.lockfile").split("\0")) {
            if (!path.isEmpty()) paths.add(path);
        }
        if (paths.isEmpty()) throw new IllegalStateException("no committed Gradle lock files");
        paths.sort(String::compareTo);
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        for (String path : paths) {
            digest.update(path.getBytes(StandardCharsets.UTF_8));
            digest.update((byte) 0);
            digest.update(Files.readAllBytes(Path.of(path)));
            digest.update((byte) 0);
        }
        return HexFormat.of().formatHex(digest.digest());
    }

    // ---- signing --------------------------------------------------------------------------------

    private static void sign(Path manifest, Path privatePem, Path out) throws Exception {
        String pem = Files.readString(privatePem, StandardCharsets.US_ASCII);
        String body = pem.replace("-----BEGIN PRIVATE KEY-----", "").replace("-----END PRIVATE KEY-----", "")
                .replaceAll("\\s", "");
        PrivateKey key = KeyFactory.getInstance("EC").generatePrivate(new PKCS8EncodedKeySpec(Base64.getDecoder().decode(body)));
        Signature signer = Signature.getInstance("SHA256withECDSA");
        signer.initSign(key);
        signer.update(Files.readAllBytes(manifest));
        Files.write(out, signer.sign());
        System.out.println("signed " + manifest.getFileName());
    }

    private static boolean verifySignature(byte[] manifest, byte[] signature, byte[] spki) throws Exception {
        PublicKey key = requireP256(spki);
        Signature verifier = Signature.getInstance("SHA256withECDSA");
        verifier.initVerify(key);
        verifier.update(manifest);
        try {
            return verifier.verify(signature);
        } catch (java.security.SignatureException malformed) {
            return false;
        }
    }

    private static PublicKey requireP256(byte[] spki) throws Exception {
        PublicKey key = KeyFactory.getInstance("EC").generatePublic(new X509EncodedKeySpec(spki));
        if (!(key instanceof ECPublicKey ec) || ec.getParams().getCurve().getField().getFieldSize() != 256) {
            throw new IllegalStateException("release public key must be ECDSA P-256");
        }
        return key;
    }

    // ---- checksums ------------------------------------------------------------------------------

    private static void writeSums(Path dist, String version) throws IOException {
        StringBuilder sums = new StringBuilder();
        for (String name : distributedFiles(version)) {
            sums.append(sha256Hex(Files.readAllBytes(dist.resolve(name)))).append("  ").append(name).append('\n');
        }
        Files.writeString(dist.resolve("SHA256SUMS.txt"), sums, StandardCharsets.US_ASCII);
        System.out.print(sums);
    }

    // ---- verification ---------------------------------------------------------------------------

    private enum Mode {
        UNSIGNED, SIGNED;

        static Mode parse(String value) {
            return switch (value) {
                case "unsigned" -> UNSIGNED;
                case "signed" -> SIGNED;
                default -> throw new IllegalStateException("mode must be unsigned or signed");
            };
        }
    }

    private static void verifyRelease(Path dist, String version, Mode mode) throws Exception {
        ReleaseConfig config = ReleaseConfig.provisioned(loadConfig());
        byte[] manifestBytes = Files.readAllBytes(dist.resolve("release.json"));
        byte[] signature = Files.readAllBytes(dist.resolve("release.json.sig"));
        byte[] testKey = Files.readAllBytes(TEST_PUBLIC_KEY);
        byte[] trusted = mode == Mode.SIGNED ? config.publicKey() : testKey;
        if (mode == Mode.SIGNED && Arrays.equals(config.publicKey(), testKey)) {
            throw new IllegalStateException("configured stable key is the test fixture key");
        }
        check(verifySignature(manifestBytes, signature, trusted), "manifest signature verifies with the " + mode.name().toLowerCase() + "-mode key");
        byte[] other = mode == Mode.SIGNED ? testKey : config.publicKey();
        check(!verifySignature(manifestBytes, signature, other), "manifest signature does not verify with the other mode's key");

        check(manifestBytes.length > 0 && manifestBytes[manifestBytes.length - 1] == '\n'
                && (manifestBytes.length < 2 || manifestBytes[manifestBytes.length - 2] != '\n')
                && !(manifestBytes.length >= 3 && manifestBytes[0] == (byte) 0xEF), "manifest is UTF-8 without BOM ending in exactly one LF");
        Map<String, Object> manifest = objectOf(new JsonParser(new String(manifestBytes, StandardCharsets.UTF_8)).parseDocument(), MANIFEST_KEYS, "manifest");
        StringBuilder canonical = new StringBuilder();
        writeJson(canonical, manifest);
        canonical.append('\n');
        check(Arrays.equals(canonical.toString().getBytes(StandardCharsets.UTF_8), manifestBytes), "manifest bytes are canonical");

        check(Long.valueOf(1).equals(manifest.get("schema_version")), "schema_version=1");
        check("stable".equals(manifest.get("channel")), "channel=stable");
        check(version.equals(manifest.get("version")), "version matches " + version);
        check(Long.valueOf(versionCode(version)).equals(manifest.get("version_code")), "version_code matches version");
        check(manifest.get("published_at") instanceof String at && PUBLISHED_AT.matcher(at).matches(), "published_at format");
        check(Long.valueOf(33).equals(manifest.get("min_android_sdk")), "min_android_sdk=33");
        check(Long.valueOf(1).equals(manifest.get("protocol_version")), "protocol_version=1");
        check(Long.valueOf(1).equals(manifest.get("store_schema_version")), "store_schema_version=1");
        check(config.notesUrl(version).equals(manifest.get("release_notes_url")), "release_notes_url names the configured release");

        Map<String, Object> provenance = objectOf(manifest.get("provenance"), PROVENANCE_KEYS, "provenance");
        check(provenance.get("source_revision") instanceof String rev && HEX40.matcher(rev).matches(), "source_revision is a 40-hex revision");
        for (var tool : TOOLS.entrySet()) check(tool.getValue().equals(provenance.get(tool.getKey())), "provenance " + tool.getKey());
        check(LIBPCAP_SOURCE_SHA256.equals(provenance.get("libpcap_source_sha256")), "libpcap source digest");
        for (String digest : List.of("libpcap_patch_sha256", "gradle_lock_sha256", "gradle_verification_sha256", "cargo_lock_sha256")) {
            check(provenance.get(digest) instanceof String value && HEX64.matcher(value).matches(), digest + " is 64 lowercase hex");
        }
        check(config.apkSignerSha256().equals(provenance.get("apk_signer_sha256")), "provenance apk_signer_sha256 matches configuration");
        check(config.releaseKeyId().equals(provenance.get("release_key_id")), "provenance release_key_id matches configuration");

        Map<String, Object> artifacts = objectOf(manifest.get("artifacts"), List.of("apk", "magisk"), "artifacts");
        verifyArtifact(dist, objectOf(artifacts.get("apk"), ARTIFACT_KEYS, "apk"), apkName(version), config.releaseBase(version));
        verifyArtifact(dist, objectOf(artifacts.get("magisk"), ARTIFACT_KEYS, "magisk"), moduleName(version), config.releaseBase(version));

        List<String> expected = distributedFiles(version);
        List<String> lines = Files.readAllLines(dist.resolve("SHA256SUMS.txt"), StandardCharsets.US_ASCII);
        check(lines.size() == expected.size(), "SHA256SUMS.txt has exactly the fixed file set");
        byte[] sumsBytes = Files.readAllBytes(dist.resolve("SHA256SUMS.txt"));
        check(indexOf(sumsBytes, (byte) '\r') < 0, "SHA256SUMS.txt uses LF endings");
        for (int i = 0; i < expected.size(); i++) {
            String name = expected.get(i);
            check((sha256Hex(Files.readAllBytes(dist.resolve(name))) + "  " + name).equals(lines.get(i)), "SHA256SUMS entry " + name);
        }

        Path apk = dist.resolve(apkName(version));
        if (mode == Mode.UNSIGNED) {
            requireUnsignedApk(apk);
            System.out.println("PASS APK carries no signature");
        }
        System.out.println("RESULT PASS verify-release " + mode.name().toLowerCase());
    }

    private static void verifyArtifact(Path dist, Map<String, Object> artifact, String name, String base) throws IOException {
        check(name.equals(artifact.get("name")), "artifact name " + name);
        check((base + name).equals(artifact.get("url")), "artifact url for " + name);
        Path file = dist.resolve(name);
        check(artifact.get("size") instanceof Long size && size >= 1 && size <= MAX_ARTIFACT_BYTES && size == Files.size(file), "artifact size for " + name);
        check(sha256Hex(Files.readAllBytes(file)).equals(artifact.get("sha256")), "artifact sha256 for " + name);
    }

    private static void requireUnsignedApk(Path apk) throws IOException {
        byte[] bytes = Files.readAllBytes(apk);
        if (indexOf(bytes, "APK Sig Block 42".getBytes(StandardCharsets.US_ASCII)) >= 0) {
            throw new IllegalStateException("APK contains an APK Signing Block");
        }
        try (ZipFile zip = new ZipFile(apk.toFile())) {
            boolean jarSignature = zip.stream().map(ZipEntry::getName)
                    .anyMatch(n -> n.startsWith("META-INF/") && (n.endsWith(".RSA") || n.endsWith(".EC") || n.endsWith(".DSA") || n.endsWith(".SF")));
            if (jarSignature) throw new IllegalStateException("APK contains a JAR signature");
        }
    }

    private static void check(boolean condition, String assertion) {
        if (!condition) throw new IllegalStateException("FAIL " + assertion);
        System.out.println("PASS " + assertion);
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> objectOf(Object value, List<String> keys, String what) {
        if (!(value instanceof Map<?, ?> map)) throw new IllegalStateException(what + " must be an object");
        if (!map.keySet().equals(new java.util.HashSet<>(keys))) {
            throw new IllegalStateException(what + " keys must be exactly " + keys + ", found " + map.keySet());
        }
        Map<String, Object> ordered = new LinkedHashMap<>();
        for (String key : keys) ordered.put(key, map.get(key));
        return ordered;
    }

    // ---- canonical JSON -------------------------------------------------------------------------

    @SuppressWarnings("unchecked")
    private static void writeJson(StringBuilder out, Object value) {
        if (value instanceof Map<?, ?> map) {
            out.append('{');
            boolean first = true;
            for (var entry : ((Map<String, Object>) map).entrySet()) {
                if (!first) out.append(',');
                first = false;
                writeString(out, entry.getKey());
                out.append(':');
                writeJson(out, entry.getValue());
            }
            out.append('}');
        } else if (value instanceof String string) {
            writeString(out, string);
        } else if (value instanceof Long number) {
            out.append(number.longValue());
        } else {
            throw new IllegalStateException("unsupported manifest value: " + value);
        }
    }

    private static void writeString(StringBuilder out, String value) {
        out.append('"');
        for (char c : value.toCharArray()) {
            switch (c) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                default -> {
                    if (c < 0x20) out.append(String.format("\\u%04x", (int) c));
                    else out.append(c);
                }
            }
        }
        out.append('"');
    }

    /** Strict JSON reader: objects, strings and non-negative integers only; duplicate keys rejected. */
    private static final class JsonParser {
        private final String text;
        private int position;

        JsonParser(String text) {
            this.text = text;
        }

        Object parseDocument() {
            Object value = parseValue();
            if (position < text.length() && text.charAt(position) == '\n') position++;
            if (position != text.length()) throw error("trailing content");
            return value;
        }

        private Object parseValue() {
            if (position >= text.length()) throw error("unexpected end");
            char c = text.charAt(position);
            if (c == '{') return parseObject();
            if (c == '"') return parseString();
            if (c >= '0' && c <= '9') return parseInteger();
            throw error("unsupported value");
        }

        private Map<String, Object> parseObject() {
            position++;
            Map<String, Object> map = new LinkedHashMap<>();
            if (peek() == '}') {
                position++;
                return map;
            }
            while (true) {
                if (peek() != '"') throw error("expected key");
                String key = parseString();
                expect(':');
                if (map.put(key, parseValue()) != null) throw error("duplicate key " + key);
                char next = peek();
                position++;
                if (next == '}') return map;
                if (next != ',') throw error("expected , or }");
            }
        }

        private String parseString() {
            position++;
            StringBuilder out = new StringBuilder();
            while (position < text.length()) {
                char c = text.charAt(position++);
                if (c == '"') return out.toString();
                if (c < 0x20) throw error("control character in string");
                if (c == '\\') {
                    char escaped = text.charAt(position++);
                    switch (escaped) {
                        case '"' -> out.append('"');
                        case '\\' -> out.append('\\');
                        case '/' -> out.append('/');
                        case 'u' -> {
                            out.append((char) Integer.parseInt(text.substring(position, position + 4), 16));
                            position += 4;
                        }
                        default -> throw error("unsupported escape");
                    }
                } else {
                    out.append(c);
                }
            }
            throw error("unterminated string");
        }

        private Long parseInteger() {
            int start = position;
            while (position < text.length() && Character.isDigit(text.charAt(position))) position++;
            String digits = text.substring(start, position);
            if (digits.length() > 1 && digits.charAt(0) == '0') throw error("leading zero");
            if (position < text.length() && ".eE".indexOf(text.charAt(position)) >= 0) throw error("non-integer number");
            return Long.parseLong(digits);
        }

        private char peek() {
            if (position >= text.length()) throw error("unexpected end");
            return text.charAt(position);
        }

        private void expect(char c) {
            if (peek() != c) throw error("expected " + c);
            position++;
        }

        private IllegalStateException error(String message) {
            return new IllegalStateException("invalid manifest JSON at " + position + ": " + message);
        }
    }

    // ---- helpers --------------------------------------------------------------------------------

    private static String git(String... args) throws Exception {
        List<String> command = new ArrayList<>();
        command.add("git");
        command.addAll(List.of(args));
        Process process = new ProcessBuilder(command).redirectErrorStream(true).start();
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        try (InputStream input = process.getInputStream()) {
            input.transferTo(output);
        }
        if (process.waitFor() != 0) throw new IllegalStateException("git " + String.join(" ", args) + " failed");
        return output.toString(StandardCharsets.UTF_8);
    }

    private static String sha256Hex(byte[] bytes) {
        try {
            return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes));
        } catch (java.security.NoSuchAlgorithmException impossible) {
            throw new IllegalStateException(impossible);
        }
    }

    private static int indexOf(byte[] haystack, byte needle) {
        for (int i = 0; i < haystack.length; i++) if (haystack[i] == needle) return i;
        return -1;
    }

    private static int indexOf(byte[] haystack, byte[] needle) {
        outer:
        for (int i = 0; i + needle.length <= haystack.length; i++) {
            for (int j = 0; j < needle.length; j++) if (haystack[i + j] != needle[j]) continue outer;
            return i;
        }
        return -1;
    }

    private static void expectArgs(String[] args, int count) {
        if (args.length != count) throw new IllegalStateException("wrong argument count for " + args[0]);
    }

    private static Map<String, String> orderedMap(String... pairs) {
        Map<String, String> map = new LinkedHashMap<>();
        for (int i = 0; i < pairs.length; i += 2) map.put(pairs[i], pairs[i + 1]);
        return map;
    }
}
