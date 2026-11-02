import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "build_predicate", Path(__file__).resolve().parents[1] / "scripts/build_predicate.py"
)
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


def repository(path):
    path.mkdir()
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    (path / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n')
    subprocess.run(["git", "add", "Cargo.toml"], cwd=path, check=True)
    subprocess.run([
        "git", "-c", "user.name=Test", "-c", "user.email=test@example.com",
        "-c", "commit.gpgsign=false", "commit", "-qm", "Create a fixture.",
    ], cwd=path, check=True)
    return builder.git(path, "rev-parse", "HEAD").decode().strip()


class BuildTests(unittest.TestCase):
    def test_environment_refuses_overrides_without_printing_values(self):
        for name in builder.FORBIDDEN_ENV | {
            "CARGO_PROFILE_RELEASE_OPT_LEVEL", "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER",
            "CARGO_UNSTABLE_BUILD_STD",
        }:
            with self.subTest(name=name):
                with self.assertRaises(ValueError) as error:
                    builder.check_environment({name: "not-for-the-error-message"})
                self.assertIn(name, str(error.exception))
                self.assertNotIn("not-for-the-error-message", str(error.exception))
        builder.check_environment({"CARGO_HOME": "/cache", "RUSTUP_TOOLCHAIN": "stable"})

    def test_config_refuses_includes_forced_environment_and_build_overrides(self):
        for config in [
            {"include": ["extra.toml"]}, {"env": {"PREDICATE_ENGINE_REVISION": "wrong"}},
            {"profile": {"release": {"panic": "abort"}}}, {"paths": ["override"]},
            {"patch": {"crates-io": {}}}, {"target": {"cfg(unix)": {"rustflags": []}}},
            {"unstable": {"build-std": True}}, {"source": {"vendored": {"directory": "vendor"}}},
            *[{"build": {key: "override"}} for key in builder.FORBIDDEN_BUILD],
        ]:
            with self.subTest(config=config), self.assertRaises(ValueError):
                builder.check_config(config)
        builder.check_config({"net": {"offline": True}, "build": {"jobs": 2}})

    def test_config_discovery_records_ancestors_and_legacy_precedence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            harness = root / "harness"
            harness.mkdir()
            cargo_home = root / "cargo-home"
            cargo_home.mkdir()
            ancestor = root / ".cargo"
            ancestor.mkdir()
            (ancestor / "config").write_text("[net]\noffline = true\n")
            (ancestor / "config.toml").write_text('[build]\nrustflags = ["inactive"]\n')
            (cargo_home / "config.toml").write_text("[build]\njobs = 2\n")
            records = builder.configuration(harness, cargo_home)
            selected = {entry["path"]: entry for entry in records}
            self.assertTrue(selected[str(ancestor / "config")]["active"])
            self.assertFalse(selected[str(ancestor / "config.toml")]["active"])
            self.assertTrue(selected[str(cargo_home / "config.toml")]["active"])
            for entry in records:
                self.assertEqual(entry["sha256"], builder.sha256(Path(entry["path"]).read_bytes()))
            (ancestor / "config").unlink()
            with self.assertRaises(ValueError):
                builder.configuration(harness, cargo_home)

    def test_dependency_paths_require_matching_engine_and_common(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            harness, engine = root / "riffle", root / "tantivy"
            metadata = {"packages": [
                {"name": "riffle", "manifest_path": str(harness / "Cargo.toml"), "source": None},
                {"name": "tantivy", "manifest_path": str(engine / "Cargo.toml"), "source": None},
                {"name": "tantivy-common", "manifest_path": str(engine / "common/Cargo.toml"), "source": None},
            ]}
            builder.verify_dependencies(metadata, harness, engine)
            metadata["packages"][2]["manifest_path"] = str(root / "other/common/Cargo.toml")
            with self.assertRaises(ValueError):
                builder.verify_dependencies(metadata, harness, engine)
            metadata["packages"][2]["manifest_path"] = str(engine / "common/Cargo.toml")
            metadata["packages"].append({"name": "unrecorded", "manifest_path": str(root / "other/Cargo.toml"), "source": None})
            with self.assertRaises(ValueError):
                builder.verify_dependencies(metadata, harness, engine)

    def test_engine_requires_exact_clean_worktree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve() / "engine"
            revision = repository(root)
            builder.clean_engine(root, revision)
            with self.assertRaises(ValueError):
                builder.clean_engine(root, "a" * 40)
            (root / "extra.rs").write_text("// Untracked source.\n")
            with self.assertRaises(ValueError):
                builder.clean_engine(root, revision)
            (root / "extra.rs").unlink()
            (root / "Cargo.toml").write_text("changed\n")
            with self.assertRaises(ValueError):
                builder.clean_engine(root, revision)

    def test_harness_records_untracked_source_and_tracked_diff_without_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve() / "harness"
            revision = repository(root)
            (root / "new.rs").write_text("fn main() {}\n")
            (root / "Cargo.toml").write_text("changed\n")
            (root / "ordinary-input.geojson").write_text("{}\n")
            source = builder.harness_source(root)
            self.assertEqual(source["revision"], revision)
            self.assertEqual([item["path"] for item in source["source_files"]], ["Cargo.toml", "new.rs"])
            self.assertIn("+changed", source["tracked_diff"])
            self.assertNotIn("ordinary-input", json.dumps(source))
            (root / "new.rs").write_text("fn changed() {}\n")
            self.assertNotEqual(source, builder.harness_source(root))


if __name__ == "__main__":
    unittest.main()
