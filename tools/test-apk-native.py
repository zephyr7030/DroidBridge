import struct
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


class ApkNativeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory(prefix="droidbridge-apk-native-")
        cls.directory = Path(cls.scratch.name)
        subprocess.run(
            ["javac", "-J-Duser.language=en", "-encoding", "UTF-8", "-d", str(cls.directory), "tools/ReleaseTool.java"],
            cwd=ROOT,
            check=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def payload(self, machines=None):
        machines = machines or {"arm64-v8a": 183, "x86_64": 62}
        result = {}
        for abi, machine in machines.items():
            header = bytearray(20)
            header[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<H", header, 18, machine)
            for library in ("libapp_native.so", "libdroidbridge_exec_guard.so"):
                result[f"lib/{abi}/{library}"] = bytes(header)
        return result

    def verify(self, payload, edition="app"):
        apk = self.directory / f"{self.id().split('.')[-1]}.apk"
        with zipfile.ZipFile(apk, "w") as archive:
            for name, content in payload.items():
                archive.writestr(name, content)
        return subprocess.run(
            ["java", "-cp", str(self.directory), "ReleaseTool", "verify-apk-native", str(apk), edition],
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )

    def test_app_accepts_both_native_payloads(self):
        result = self.verify(self.payload())
        self.assertEqual(0, result.returncode, result.stderr)

    def test_app_rejects_arm_only(self):
        result = self.verify(self.payload({"arm64-v8a": 183}))
        self.assertNotEqual(0, result.returncode)
        self.assertIn("exact native ABI set", result.stderr)

    def test_app_rejects_missing_x86_guard(self):
        payload = self.payload()
        del payload["lib/x86_64/libdroidbridge_exec_guard.so"]
        result = self.verify(payload)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("App carries x86_64/libdroidbridge_exec_guard.so", result.stderr)

    def test_app_rejects_arm_elf_under_x86_path(self):
        result = self.verify(self.payload({"arm64-v8a": 183, "x86_64": 183}))
        self.assertNotEqual(0, result.returncode)
        self.assertIn("ELF machine matches lib/x86_64/", result.stderr)

    def test_app_rejects_truncated_elf(self):
        payload = self.payload()
        payload["lib/x86_64/libapp_native.so"] = b"\x7fELF"
        result = self.verify(payload)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("64-bit little-endian ELF", result.stderr)

    def test_app_rejects_32_bit_x86(self):
        result = self.verify(self.payload({"arm64-v8a": 183, "x86_64": 62, "x86": 3}))
        self.assertNotEqual(0, result.returncode)
        self.assertIn("supported native ABI x86", result.stderr)

    def test_root_accepts_arm_and_rejects_x86(self):
        result = self.verify(self.payload({"arm64-v8a": 183}), "root")
        self.assertEqual(0, result.returncode, result.stderr)
        result = self.verify(self.payload(), "root")
        self.assertNotEqual(0, result.returncode)
        self.assertIn("supported native ABI x86_64", result.stderr)


if __name__ == "__main__":
    unittest.main()
