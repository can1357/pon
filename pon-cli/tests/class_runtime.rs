use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const CLASS_CORPUS: &str = r#"class Point:
    def __init__(self, x):
        self.x = x

p = Point(7)
print(p.x)
"#;

#[test]
fn run_executes_class_corpus_fixture() {
    let fixture_dir = TempDir::new("pon-cli-run-classes");
    let fixture_path = fixture_dir.path().join("classes.py");
    fs::write(&fixture_path, CLASS_CORPUS).expect("write classes.py fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_pon-cli"))
        .arg("run")
        .arg(&fixture_path)
        .output()
        .expect("run pon-cli binary");

    assert!(
        output.status.success(),
        "pon run should exit successfully; status={:?}, stdout={}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.as_slice(), b"7\n");
    assert_eq!(output.stderr.as_slice(), b"");
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

        for _ in 0..1000 {
            let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("{prefix}-{}-{suffix}", std::process::id()));

            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("create test fixture directory {path:?}: {err}"),
            }
        }

        panic!("could not create a unique temporary directory for {prefix}");
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
