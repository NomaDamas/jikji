use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub(crate) struct TempRoot {
    path: PathBuf,
}

impl std::ops::Deref for TempRoot {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const REQUIRED_SCENARIOS: &[&str] = &[
    "ascii_cjk_paths",
    "structured_archive_media",
    "answer_pack_shell",
    "stale_index_find",
    "clean_safety",
];

pub(crate) fn assert_golden_manifest() {
    let manifest_path = golden_root().join("manifest.json");
    assert!(
        manifest_path.is_file(),
        "parity/golden mismatch: missing Python golden manifest at {}",
        manifest_path.display()
    );
    let manifest = fs::read_to_string(&manifest_path).unwrap_or_else(|error| {
        panic!(
            "parity/golden mismatch: cannot read {}: {error}",
            manifest_path.display()
        )
    });
    for scenario in REQUIRED_SCENARIOS {
        assert!(
            manifest.contains(scenario),
            "parity/golden mismatch: Python golden manifest is missing scenario {scenario}"
        );
    }
}

pub(crate) fn golden_command(scenario: &str, name: &str) -> Value {
    let commands_path = golden_root()
        .join("scenarios")
        .join(scenario)
        .join("commands.json");
    let raw = fs::read_to_string(&commands_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", commands_path.display()));
    let commands: Value = serde_json::from_str(&raw).expect("golden commands json");
    commands
        .as_array()
        .expect("golden command array")
        .iter()
        .find(|command| command["name"] == name)
        .and_then(|command| command.get("stdout_json"))
        .cloned()
        .unwrap_or_else(|| panic!("missing golden stdout_json for {scenario}/{name}"))
}

pub(crate) fn run_json(args: &[&str]) -> Value {
    let output = run(args);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json stdout")
}

pub(crate) fn run(args: &[&str]) -> Output {
    let mut command = Command::new(rust_bin());
    command.args(args);
    if let Some(root) = args.get(1) {
        command.env("JIKJI_DATA_DIR", data_dir_for(root));
    }
    command.output().expect("run rust jikji")
}

pub(crate) fn temp_root(label: &str) -> TempRoot {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "jikji-parity-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create temp root");
    TempRoot { path: root }
}

pub(crate) fn root_arg(root: &Path) -> String {
    root.display().to_string()
}

pub(crate) fn database_path(root: &Path) -> PathBuf {
    data_dir_for(root.to_string_lossy().as_ref()).join("jikji/index.sqlite")
}

fn data_dir_for(root: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    std::env::temp_dir().join(format!("jikji-parity-db-{:016x}", hasher.finish()))
}

pub(crate) fn write_ascii_cjk_fixture(root: &Path) {
    fs::create_dir_all(root.join("docs")).expect("create docs");
    fs::create_dir_all(root.join("자료")).expect("create cjk folder");
    fs::write(
        root.join("docs/acme-contract.txt"),
        "ACME renewal contract contains indemnity marker direct-answer-771",
    )
    .expect("write acme");
    fs::write(
        root.join("자료/회의록.txt"),
        "서울 연구소 회의록 contains cjk-marker-902",
    )
    .expect("write cjk");
}

pub(crate) fn write_answer_pack_fixture(root: &Path) {
    fs::create_dir_all(root.join("contracts")).expect("create contracts");
    fs::write(
        root.join("contracts/acme-renewal.txt"),
        "unique renewal indemnity clause direct-pack-445",
    )
    .expect("write renewal");
    fs::write(root.join("notes.txt"), "unrelated notes").expect("write notes");
}

fn rust_bin() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let status = Command::new("cargo")
            .args(["build", "--quiet", "-p", "jikji-cli", "--bin", "jikji"])
            .current_dir(workspace_root())
            .status()
            .expect("cargo build jikji");
        assert!(status.success(), "failed to build Rust CLI binary");
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    workspace_root().join(path)
                }
            })
            .unwrap_or_else(|| workspace_root().join("target"));
        target_dir
            .join("debug")
            .join(format!("jikji{}", std::env::consts::EXE_SUFFIX))
    })
}

fn golden_root() -> PathBuf {
    workspace_root().join("tests/golden/python")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("jikji-parity crate must live under crates/jikji-parity")
        .to_path_buf()
}
