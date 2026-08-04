import unittest
import tempfile
import csv
from contextlib import redirect_stdout
import io
from pathlib import Path
import subprocess
import sys
from unittest.mock import patch

import corpus
import execution_inventory


class TargetedBuildTests(unittest.TestCase):
    @staticmethod
    def pin(case, isa="aarch64", digest="old"):
        return {"suite": "process", "case": case, "isa": isa, "sha256": digest}

    def test_unselected_pin_survives(self):
        retained = self.pin("retained")
        old = self.pin("selected")
        new = self.pin("selected", digest="new")
        merged = corpus.merge_pins([retained, old], [new])
        self.assertEqual(merged, [retained, new])

    def test_both_isa_pins_survive_single_replacement(self):
        arm = self.pin("edge")
        x86 = self.pin("edge", "x86_64")
        replacement = self.pin("other", digest="new")
        merged = corpus.merge_pins([arm, x86], [replacement])
        self.assertEqual(merged, [arm, x86, replacement])

    def test_local_manifests_merge(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / "oracle"
            local = root / "local"
            suite = destination / "process"
            suite.mkdir(parents=True)
            header = "# case\tgroup\tsource\tisas\tcflags\texit\tstdout\tdisposition"
            (suite / "manifest.tsv").write_text(
                f"{header}\nbase\tprocess\tbase.c\taarch64\t-static\t0\tbase.out\tactive\n"
            )
            first = local / "process"
            first.mkdir(parents=True)
            (first / "manifest.tsv").write_text(
                f"{header}\nnamespace-boundary\tprocess\tns.c\taarch64\t-static\t0\tns.out\tactive\n"
            )
            corpus.overlay_local(destination, local)
            second = root / "second" / "process"
            second.mkdir(parents=True)
            (second / "manifest.tsv").write_text(
                f"{header}\ninet-loopback\tprocess\tinet.c\tx86_64\t-static\t0\tinet.out\tactive\n"
            )
            corpus.overlay_local(destination, root / "second")
            text = (suite / "manifest.tsv").read_text()
            self.assertIn("base\tprocess", text)
            self.assertIn("namespace-boundary\tprocess", text)
            self.assertIn("inet-loopback\tprocess", text)

    def test_hierarchical_suite_prevents_case_collision(self):
        rows = [
            {"suite": "abi", "case": "shared", "isa": "aarch64", "state": "build"},
            {"suite": "core/abi", "case": "shared", "isa": "aarch64", "state": "build"},
        ]
        selected = corpus.select_rows(rows, [], {"shared"}, {"core/abi"}, False)
        self.assertEqual([(row["suite"], row["case"]) for row in selected],
                         [("core/abi", "shared")])

    def test_missing_selection_resumes_from_pins(self):
        rows = [
            {"suite": "core/abi", "case": "old", "isa": "aarch64", "state": "build"},
            {"suite": "core/abi", "case": "new", "isa": "aarch64", "state": "build"},
        ]
        pins = [{"suite": "core/abi", "case": "old", "isa": "aarch64"}]
        selected = corpus.select_rows(rows, pins, None, {"core/abi"}, True)
        self.assertEqual([row["case"] for row in selected], ["new"])

    def test_interruption_preserves_artifact_and_removes_temporary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            oracle = root / "oracle"
            artifacts = root / "artifacts"
            source = oracle / "tests/compat/core/abi/probe.c"
            source.parent.mkdir(parents=True)
            source.write_text("int main(void) { return 0; }\n")
            output = artifacts / "core/abi/aarch64/probe"
            output.parent.mkdir(parents=True)
            output.write_bytes(b"prior")
            record = {
                "suite": "core/abi", "case": "probe", "isa": "aarch64",
                "source": "tests/compat/core/abi/probe.c", "state": "build",
                "cflags": "-static", "exit": "0", "stdout": "-",
            }
            with patch.multiple(corpus, ORACLE=oracle, ARTIFACTS=artifacts), \
                    patch.object(corpus, "compiler", return_value="cc"), \
                    patch.object(corpus.subprocess, "run", side_effect=KeyboardInterrupt):
                with self.assertRaises(KeyboardInterrupt):
                    corpus.build_one(record)
            self.assertEqual(output.read_bytes(), b"prior")
            self.assertEqual(list(output.parent.glob(".*.tmp")), [])

    def test_prebuilt_source_is_copied_with_bytes_and_mode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            oracle = root / "oracle"
            artifacts = root / "artifacts"
            source = oracle / "tests/compat/core/regress/guest"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"prebuilt guest\0bytes")
            source.chmod(0o751)
            record = {
                "suite": "core/regress", "case": "guest", "isa": "aarch64",
                "source": "tests/compat/core/regress/guest", "state": "build",
                "cflags": "prebuilt external static", "exit": "0", "stdout": "-",
            }
            with patch.multiple(corpus, ROOT=root, ORACLE=oracle, ARTIFACTS=artifacts), \
                    patch.object(corpus, "compiler") as compiler:
                result = corpus.build_one(record)
            output = artifacts / "core/regress/aarch64/guest"
            compiler.assert_not_called()
            self.assertEqual(output.read_bytes(), source.read_bytes())
            self.assertEqual(output.stat().st_mode & 0o7777, 0o751)
            self.assertEqual(result["toolchain"], "prebuilt-copy")
            self.assertEqual(result["sha256"], corpus.digest(source))
            self.assertEqual(result["source_sha256"], corpus.digest(source))
            self.assertEqual(result["recipe_sha256"], corpus.text_digest(record["cflags"]))

    def test_prebuilt_interruption_preserves_prior_pin_and_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            oracle = root / "oracle"
            artifacts = root / "artifacts"
            source = oracle / "tests/compat/core/regress/guest"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"replacement")
            source.chmod(0o755)
            output = artifacts / "core/regress/aarch64/guest"
            output.parent.mkdir(parents=True)
            output.write_bytes(b"prior")
            prior = self.pin("guest")
            record = {
                "suite": "core/regress", "case": "guest", "isa": "aarch64",
                "source": "tests/compat/core/regress/guest", "state": "build",
                "cflags": "prebuilt", "exit": "0", "stdout": "-",
            }
            with patch.multiple(corpus, ROOT=root, ORACLE=oracle, ARTIFACTS=artifacts), \
                    patch.object(corpus.os, "replace", side_effect=KeyboardInterrupt):
                with self.assertRaises(KeyboardInterrupt):
                    corpus.build_one(record)
            self.assertEqual(output.read_bytes(), b"prior")
            self.assertEqual(list(output.parent.glob(".*.tmp")), [])
            self.assertEqual(corpus.merge_pins([prior], []), [prior])

    def test_c_source_still_compiles(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            oracle = root / "oracle"
            artifacts = root / "artifacts"
            source = oracle / "tests/compat/core/abi/probe.c"
            source.parent.mkdir(parents=True)
            source.write_text("int main(void) { return 0; }\n")
            record = {
                "suite": "core/abi", "case": "probe", "isa": "aarch64",
                "source": "tests/compat/core/abi/probe.c", "state": "build",
                "cflags": "-static -lprobe", "exit": "0", "stdout": "-",
            }

            def run(command, **_):
                if "-dumpfullversion" in command:
                    return type("Version", (), {
                        "returncode": 0, "stderr": "", "stdout": "1.2.3\n",
                    })()
                Path(command[command.index("-o") + 1]).write_bytes(b"compiled")
                return type("Completed", (), {
                    "returncode": 0, "stderr": "", "stdout": "",
                })()

            with patch.multiple(corpus, ROOT=root, ORACLE=oracle, ARTIFACTS=artifacts), \
                    patch.object(corpus, "compiler", return_value="cc"), \
                    patch.object(corpus.subprocess, "run", side_effect=run):
                result = corpus.build_one(record)
            self.assertEqual(result["state"], "built")
            self.assertEqual((artifacts / "core/abi/aarch64/probe").read_bytes(), b"compiled")
            self.assertEqual(result["toolchain"], "cc-1.2.3")
            self.assertEqual(result["cflags"], "-static -lprobe")
            self.assertEqual(result["recipe_sha256"], corpus.text_digest("-static -lprobe"))

    def test_static_libraries_follow_source_and_output(self):
        source = Path("probe.c")
        output = Path("probe")
        command = corpus.compiler_command(
            "cc", "-static-pie -O2 -pthread -lsqlite3 -lm -ldl", source, output
        )
        self.assertEqual(
            command,
            [
                "cc", "-static-pie", "-O2", "-pthread", "probe.c", "-o", "probe",
                "-lsqlite3", "-lm", "-ldl",
            ],
        )
        self.assertLess(command.index("probe.c"), command.index("-lsqlite3"))

    def test_static_compiler_contract_is_preserved(self):
        configured = "aarch64-linux-gnu-gcc -I/store/static/include -L/store/static/lib"
        with patch.object(
            corpus.os,
            "environ",
            {"AARCH64_LINUX_STATIC_CC": configured},
        ):
            compiler = corpus.compiler("aarch64")
        self.assertEqual(
            compiler,
            [
                "aarch64-linux-gnu-gcc",
                "-I/store/static/include",
                "-L/store/static/lib",
            ],
        )

    def test_dynamic_recipe_avoids_static_search_path(self):
        environment = {
            "AARCH64_LINUX_STATIC_CC": "cc -L/static",
            "AARCH64_LINUX_CC": "cc -L/shared",
        }
        with patch.object(corpus.os, "environ", environment):
            dynamic = corpus.compiler("aarch64", "-no-pie -rdynamic -ldl")
            static = corpus.compiler("aarch64", "-static -pthread")
        self.assertEqual(dynamic, ["cc", "-L/shared"])
        self.assertEqual(static, ["cc", "-L/static"])

    def test_pin_table_replacement_is_atomic(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.tsv"
            path.write_text("prior\n")
            with patch.object(corpus.os, "replace", side_effect=KeyboardInterrupt):
                with self.assertRaises(KeyboardInterrupt):
                    corpus.write_table(path, ["case"], [{"case": "next"}])
            self.assertEqual(path.read_text(), "prior\n")
            self.assertEqual(list(path.parent.glob(".manifest.tsv.*.tmp")), [])

    def test_cmake_artifact_drift_is_detected_by_source_and_isa(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build = root / "build"
            rust = root / "compat"
            c_artifact = build / "compat/abi-corpus/x86_64/fp_fma_dnan"
            rust_artifact = rust / "artifacts/full/abi/corpus/x86_64/fp_fma_dnan"
            c_artifact.parent.mkdir(parents=True)
            rust_artifact.parent.mkdir(parents=True)
            c_artifact.write_bytes(b"CMake oracle bytes")
            rust_artifact.write_bytes(b"independently rebuilt bytes")
            source = root / "engine/tests/compat/abi/corpus/fp_fma_dnan.c"
            source.parent.mkdir(parents=True)
            source.write_text("int main(void) { return 0; }\n")
            (build / "build.ninja").write_text(
                "build compat/abi-corpus/x86_64/fp_fma_dnan: CUSTOM_COMMAND "
                f"{source}\n"
            )
            pin = {
                "suite": "abi/corpus", "case": "fp_fma_dnan", "isa": "x86_64",
                "source": "tests/compat/abi/corpus/fp_fma_dnan.c",
                "artifact": "artifacts/full/abi/corpus/x86_64/fp_fma_dnan",
            }
            with patch.object(corpus, "ROOT", rust):
                rows = corpus.parity_rows(build, [pin])
                self.assertEqual(rows[0]["state"], "different")
                rust_artifact.write_bytes(c_artifact.read_bytes())
                rows = corpus.parity_rows(build, [pin])
                self.assertEqual(rows[0]["state"], "identical")

    def cmake_import_fixture(self, root, case="fp_fma_dnan"):
        compat = root / "compat"
        oracle = compat / "oracle"
        artifacts = compat / "artifacts/full"
        manifest = compat / "artifacts/manifest.tsv"
        source = oracle / f"tests/compat/abi/corpus/{case}.c"
        source.parent.mkdir(parents=True)
        source.write_text("int main(void) { return 0; }\n")
        target = artifacts / f"abi/corpus/x86_64/{case}"
        target.parent.mkdir(parents=True)
        target.write_bytes(b"old guest")
        target.chmod(0o700)
        build = root / "build"
        c_artifact = build / f"compat/abi-corpus/x86_64/{case}"
        c_artifact.parent.mkdir(parents=True)
        c_artifact.write_bytes(b"CMake oracle guest")
        c_artifact.chmod(0o751)
        (build / "build.ninja").write_text(
            f"build compat/abi-corpus/x86_64/{case}: CUSTOM_COMMAND {source}\n"
            f"  COMMAND = cc -static {source} -o {c_artifact}\n"
        )
        pin = {
            "suite": "abi/corpus", "case": case, "isa": "x86_64",
            "artifact": f"artifacts/full/abi/corpus/x86_64/{case}",
            "sha256": corpus.digest(target), "size": str(target.stat().st_size),
            "toolchain": "prior", "source": f"tests/compat/abi/corpus/{case}.c",
            "source_sha256": corpus.digest(source), "cflags": "-static -O2 -lm",
            "recipe_sha256": corpus.text_digest("-static -O2 -lm"),
            "exit": "0", "stdout": "expected.out",
        }
        corpus.write_table(manifest, corpus.PIN_FIELDS, [pin])
        return compat, oracle, artifacts, manifest, build, c_artifact, target

    def test_cmake_import_is_idempotent_and_preserves_mode(self):
        with tempfile.TemporaryDirectory() as directory:
            values = self.cmake_import_fixture(Path(directory))
            compat, oracle, artifacts, manifest, build, c_artifact, target = values
            with patch.multiple(
                corpus, ROOT=compat, ORACLE=oracle, ARTIFACTS=artifacts,
                BUILT=manifest,
            ):
                corpus.import_cmake(build, {"fp_fma_dnan"})
                first = manifest.read_bytes()
                corpus.import_cmake(build, {"fp_fma_dnan"})
            self.assertEqual(target.read_bytes(), c_artifact.read_bytes())
            self.assertEqual(target.stat().st_mode & 0o7777, 0o751)
            self.assertEqual(manifest.read_bytes(), first)

    def test_cmake_import_interruption_rolls_back_artifact_and_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            values = self.cmake_import_fixture(Path(directory))
            compat, oracle, artifacts, manifest, build, _, target = values
            old_artifact = target.read_bytes()
            old_manifest = manifest.read_bytes()
            replace = corpus.os.replace
            interrupted = False

            def interrupt_after_artifact(source, destination):
                nonlocal interrupted
                replace(source, destination)
                if Path(destination) == target and not interrupted:
                    interrupted = True
                    raise KeyboardInterrupt

            with patch.multiple(
                corpus, ROOT=compat, ORACLE=oracle, ARTIFACTS=artifacts,
                BUILT=manifest,
            ), patch.object(corpus.os, "replace", side_effect=interrupt_after_artifact):
                with self.assertRaises(KeyboardInterrupt):
                    corpus.import_cmake(build, {"fp_fma_dnan"})
            self.assertEqual(target.read_bytes(), old_artifact)
            self.assertEqual(manifest.read_bytes(), old_manifest)
            self.assertEqual(list(target.parent.glob(".*.import")), [])
            self.assertEqual(list(target.parent.glob(".*.backup")), [])

    def test_cmake_import_rejects_artifact_target_collision(self):
        with tempfile.TemporaryDirectory() as directory:
            values = self.cmake_import_fixture(Path(directory), "first")
            compat, oracle, artifacts, manifest, build, _, _ = values
            source = oracle / "tests/compat/abi/corpus/second.c"
            source.write_text("int main(void) { return 0; }\n")
            c_artifact = build / "compat/abi-corpus/x86_64/second"
            c_artifact.write_bytes(b"second")
            with (build / "build.ninja").open("a") as graph:
                graph.write(
                    f"build compat/abi-corpus/x86_64/second: CUSTOM_COMMAND {source}\n"
                    f"  COMMAND = cc {source} -o {c_artifact}\n"
                )
            with manifest.open(newline="") as stream:
                pins = list(csv.DictReader(stream, delimiter="\t"))
            second = dict(pins[0], case="second", source="tests/compat/abi/corpus/second.c")
            second["source_sha256"] = corpus.digest(source)
            corpus.write_table(manifest, corpus.PIN_FIELDS, [pins[0], second])
            prior = manifest.read_bytes()
            with patch.multiple(
                corpus, ROOT=compat, ORACLE=oracle, ARTIFACTS=artifacts,
                BUILT=manifest,
            ):
                with self.assertRaisesRegex(SystemExit, "collision"):
                    corpus.import_cmake(build)
            self.assertEqual(manifest.read_bytes(), prior)

    def test_cmake_import_unique_skips_source_drift_and_imports_valid_row(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            values = self.cmake_import_fixture(root, "valid")
            compat, oracle, artifacts, manifest, build, valid_c, valid_target = values
            different = oracle / "tests/compat/abi/corpus/different.c"
            different.write_text("int main(void) { return 0; }\n")
            c_source = root / "engine/tests/compat/abi/corpus/different.c"
            c_source.parent.mkdir(parents=True)
            c_source.write_text("int main(void) { return 1; }\n")
            c_artifact = build / "compat/abi-corpus/x86_64/different"
            c_artifact.write_bytes(b"different CMake guest")
            with (build / "build.ninja").open("a") as graph:
                graph.write(
                    f"build compat/abi-corpus/x86_64/different: CUSTOM_COMMAND {c_source}\n"
                    f"  COMMAND = cc {c_source} -o {c_artifact}\n"
                )
            target = artifacts / "abi/corpus/x86_64/different"
            target.write_bytes(b"old different guest")
            with manifest.open(newline="") as stream:
                pins = list(csv.DictReader(stream, delimiter="\t"))
            second = dict(
                pins[0], case="different",
                artifact="artifacts/full/abi/corpus/x86_64/different",
                source="tests/compat/abi/corpus/different.c",
                source_sha256=corpus.digest(different), sha256=corpus.digest(target),
                size=str(target.stat().st_size),
            )
            corpus.write_table(manifest, corpus.PIN_FIELDS, [pins[0], second])
            with patch.multiple(
                corpus, ROOT=compat, ORACLE=oracle, ARTIFACTS=artifacts,
                BUILT=manifest,
            ):
                with self.assertRaisesRegex(SystemExit, "source-different"):
                    corpus.import_cmake(build)
                self.assertEqual(valid_target.read_bytes(), b"old guest")
                output = io.StringIO()
                with redirect_stdout(output):
                    corpus.import_cmake(build, import_unique=True)
            self.assertEqual(valid_target.read_bytes(), valid_c.read_bytes())
            self.assertEqual(target.read_bytes(), b"old different guest")
            self.assertIn("imported=1 verified=1 refused=1 source-different=1", output.getvalue())

    def test_cmake_import_classifies_missing_and_prebuilt_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            values = self.cmake_import_fixture(root, "missing")
            compat, oracle, _, manifest, build, _, _ = values
            with manifest.open(newline="") as stream:
                missing = next(csv.DictReader(stream, delimiter="\t"))
            (oracle / missing["source"]).unlink()
            prebuilt = oracle / "tests/compat/isa/x86_64/blob"
            prebuilt.parent.mkdir(parents=True)
            prebuilt.write_bytes(b"prebuilt")
            c_prebuilt = root / "engine/tests/compat/isa/x86_64/blob"
            c_prebuilt.parent.mkdir(parents=True)
            c_prebuilt.write_bytes(b"prebuilt")
            c_artifact = build / "compat/isa/x86_64/blob"
            c_artifact.parent.mkdir(parents=True)
            c_artifact.write_bytes(b"prebuilt")
            with (build / "build.ninja").open("a") as graph:
                graph.write(
                    f"build compat/isa/x86_64/blob: CUSTOM_COMMAND {c_prebuilt}\n"
                    f"  COMMAND = cp {c_prebuilt} {c_artifact}\n"
                )
            pin = dict(
                missing, suite="isa/x86_64", case="blob",
                artifact="artifacts/full/isa/x86_64/x86_64/blob",
                source="tests/compat/isa/x86_64/blob",
                source_sha256=corpus.digest(prebuilt),
            )
            with patch.multiple(corpus, ROOT=compat, ORACLE=oracle):
                rows = corpus.import_rows(build, [missing, pin])
            self.assertEqual(
                [row["import_state"] for row in rows],
                ["source-missing", "source-prebuilt"],
            )

    def transaction_process(
        self, manifest, staged, target, manifest_stage, phase=None,
    ):
        script = """
import os
from pathlib import Path
import signal
import sys
sys.path.insert(0, sys.argv[1])
import corpus
manifest, staged, target, manifest_stage = map(Path, sys.argv[2:6])
phase = sys.argv[6]
with corpus.import_lock(manifest):
    if phase == "recover":
        raise SystemExit(0)
    def fault(mark):
        if mark == phase:
            os.kill(os.getpid(), signal.SIGKILL)
    corpus.replace_transaction([(staged, target)], manifest_stage, manifest, fault)
"""
        return subprocess.run([
            sys.executable, "-c", script, str(Path(corpus.__file__).parent),
            str(manifest), str(staged), str(target), str(manifest_stage),
            phase or "recover",
        ], capture_output=True, text=True)

    def test_cmake_import_recovers_hard_exit_at_every_publication_phase(self):
        phases = {
            "planned": b"old", "prepared": b"old", "artifact": b"old",
            "artifacts": b"old", "manifest": b"old", "committed": b"new",
            "cleanup": b"new",
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for phase, expected in phases.items():
                with self.subTest(phase=phase):
                    case = root / phase
                    case.mkdir()
                    target = case / "guest"
                    target.write_bytes(b"old")
                    staged = case / ".guest.tx.cmake-import-stage"
                    staged.write_bytes(b"new")
                    manifest = case / "manifest.tsv"
                    manifest.write_bytes(b"old")
                    manifest_stage = case / ".manifest.tx.cmake-import-stage"
                    manifest_stage.write_bytes(b"new")
                    crashed = self.transaction_process(
                        manifest, staged, target, manifest_stage, phase,
                    )
                    self.assertLess(crashed.returncode, 0)
                    recovered = self.transaction_process(
                        manifest, staged, target, manifest_stage,
                    )
                    self.assertEqual(recovered.returncode, 0, recovered.stderr)
                    self.assertEqual(target.read_bytes(), expected)
                    self.assertEqual(manifest.read_bytes(), expected)
                    self.assertFalse((case / ".cmake-import.json").exists())
                    self.assertEqual(list(case.glob("*.cmake-import-stage")), [])
                    self.assertEqual(list(case.glob("*.cmake-import-backup")), [])

    def test_cmake_import_lock_refuses_concurrent_owner(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.tsv"
            manifest.write_text("old\n")
            staged = root / "unused-stage"
            target = root / "unused-target"
            with corpus.import_lock(manifest):
                blocked = self.transaction_process(
                    manifest, staged, target, staged,
                )
            self.assertNotEqual(blocked.returncode, 0)
            self.assertIn("another CMake artifact import is active", blocked.stderr)

    def test_cmake_import_removes_prejournal_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = root / "artifacts/full"
            artifacts.mkdir(parents=True)
            manifest = root / "artifacts/manifest.tsv"
            manifest.write_text("old\n")
            artifact_stage = artifacts / ".guest.tx.cmake-import-stage"
            manifest_stage = manifest.parent / ".manifest.tx.cmake-import-stage"
            artifact_stage.write_text("partial")
            manifest_stage.write_text("partial")
            corpus.cleanup_orphans(artifacts, manifest)
            self.assertFalse(artifact_stage.exists())
            self.assertFalse(manifest_stage.exists())

class ImportDiscoveryTests(unittest.TestCase):
    HEADER = "# case\tgroup\tsource\tlegacy_source\tisas\tcflags\tdefines\tenv\texit\tstdout\tdependencies\tdisposition\tnote"

    @classmethod
    def current(cls, case: str) -> str:
        return (f"{cls.HEADER}\n{case}\ttest\t{case}.c\t-\taarch64,x86_64\t-static\t-\t-\t0\t"
                f"expected/{case}.out\tlinux-libc\tactive\ttest\n")

    def import_fixture(self, retained: Path, output: Path) -> list[dict[str, str]]:
        oracle = output / "oracle"
        plan = output / "build-plan.tsv"
        with patch.multiple(corpus, ORACLE=oracle, INVENTORY=plan, LOCAL=output / "absent"):
            corpus.import_corpus(retained)
        with plan.open(newline="") as stream:
            return list(csv.DictReader(stream, delimiter="\t"))

    def test_recursive_suite_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "retained/tests/compat/core/abi/manifest.tsv"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(self.current("nested"))
            rows = self.import_fixture(root / "retained", root / "output")
            self.assertEqual({row["suite"] for row in rows}, {"core/abi"})
            self.assertEqual({row["source"] for row in rows},
                             {"tests/compat/core/abi/nested.c"})

    def test_legacy_abi_schema(self):
        lines = [
            "# legacy schema\n",
            "alloca.c\tlegacy/alloca.c\taarch64,x86_64\t0\tgolden/alloca.out\tsha256:abc;bytes:1\tportable-c\n",
        ]
        rows = corpus.legacy_rows("abi", lines)
        self.assertIsNotNone(rows)
        self.assertEqual(rows[0]["case"], "alloca")
        self.assertEqual(rows[0]["cflags"], "-static -O2 -lm")
        self.assertIsNone(corpus.legacy_rows("core/abi", lines))

    def test_soak_is_sibling_suite(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            compat = root / "retained/tests/compat/base/manifest.tsv"
            compat.parent.mkdir(parents=True)
            compat.write_text(self.current("base"))
            soak = root / "retained/tests/soak/manifest.tsv"
            soak.parent.mkdir(parents=True)
            soak.write_text(self.current("endurance"))
            rows = self.import_fixture(root / "retained", root / "output")
            soak_rows = [row for row in rows if row["suite"] == "soak"]
            self.assertEqual(len(soak_rows), 2)
            self.assertEqual({row["source"] for row in soak_rows},
                             {"tests/soak/endurance.c"})

    def test_host_exclusion_remains_buildable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "retained/tests/compat/process/manifest.tsv"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(
                self.current("linux-only").replace("\tactive\t", "\texcluded-macos\t")
            )
            rows = self.import_fixture(root / "retained", root / "output")
            self.assertEqual({row["state"] for row in rows}, {"build"})
            self.assertEqual({row["disposition"] for row in rows}, {"excluded-macos"})


class ExecutionInventoryTests(unittest.TestCase):
    @staticmethod
    def complete_root(root: Path, suite: str) -> None:
        (root / "artifacts").mkdir()
        (root / "build-plan.tsv").write_text(
            "suite\tcase\tisa\tsource\tcflags\texit\tstdout\tdefines\tenv\tdependencies\tdisposition\tnote\tstate\treason\n"
            f"{suite}\tprobe\taarch64\tprobe.c\t-static\t0\t-\t-\t-\tlinux-libc\tactive\ttest\tbuild\t-\n"
        )
        (root / "artifacts/manifest.tsv").write_text(
            "suite\tcase\tisa\tartifact\n"
            f"{suite}\tprobe\taarch64\tartifacts/{suite}/aarch64/probe\n"
        )
        (root / "inventory.tsv").write_text("suite\tcase\tisa\n")

    def test_default_timeout_matches_retained_matrix_runner(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.complete_root(root, "process")
            rows = list(csv.DictReader(execution_inventory.render(root).splitlines(), delimiter="\t"))
            self.assertEqual(rows[0]["timeout_ms"], "120000")

    def test_soak_timeout_matches_retained_cmake_override(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.complete_root(root, "soak")
            rows = list(csv.DictReader(execution_inventory.render(root).splitlines(), delimiter="\t"))
            self.assertEqual(rows[0]["timeout_ms"], "240000")

    def test_missing_pin_cannot_shrink_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "artifacts").mkdir()
            (root / "build-plan.tsv").write_text(
                "suite\tcase\tisa\tstate\nprocess\tmissing\taarch64\tbuild\n"
            )
            (root / "artifacts/manifest.tsv").write_text("suite\tcase\tisa\n")
            with self.assertRaisesRegex(ValueError, "missing=1"):
                execution_inventory.render(root)


if __name__ == "__main__":
    unittest.main()
