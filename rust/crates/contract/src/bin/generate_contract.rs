use contract::{GENERATED_ROOT, KOTLIN_FIXTURE_PATH, generated_artifacts};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("contract crate is nested under rust/crates")
        .to_path_buf()
}

fn main() -> ExitCode {
    let check = env::args().skip(1).any(|arg| arg == "--check");
    let output_root = repository_root().join(GENERATED_ROOT);
    let mut stale = false;
    for artifact in generated_artifacts() {
        let path = output_root.join(artifact.relative_path);
        if check {
            match fs::read(&path) {
                Ok(bytes) if bytes == artifact.bytes => {}
                _ => {
                    eprintln!("stale generated artifact: {}", path.display());
                    stale = true;
                }
            }
        } else {
            fs::create_dir_all(path.parent().expect("artifact has parent"))
                .expect("create artifact directory");
            fs::write(&path, artifact.bytes).expect("write generated artifact");
            println!("generated {}", path.display());
        }
    }
    let kotlin_bytes = generated_artifacts()
        .into_iter()
        .find(|artifact| artifact.relative_path == "kotlin-envelope-fixtures.v1.json")
        .expect("Kotlin fixture is generated")
        .bytes;
    let kotlin_path = repository_root().join(KOTLIN_FIXTURE_PATH);
    if check {
        match fs::read(&kotlin_path) {
            Ok(bytes) if bytes == kotlin_bytes => {}
            _ => {
                eprintln!("stale generated artifact: {}", kotlin_path.display());
                stale = true;
            }
        }
    } else {
        fs::create_dir_all(kotlin_path.parent().expect("Kotlin fixture has parent"))
            .expect("create Kotlin fixture directory");
        fs::write(&kotlin_path, kotlin_bytes).expect("write generated Kotlin fixture");
        println!("generated {}", kotlin_path.display());
    }
    if stale {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
