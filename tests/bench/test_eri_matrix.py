import importlib.util
import pathlib
import tempfile
import unittest

MODULE = pathlib.Path(__file__).with_name("eri_matrix.py")
SPEC = importlib.util.spec_from_file_location("eri_matrix", MODULE)
eri = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(eri)


class MatrixTests(unittest.TestCase):
    def test_crossed_schedule_is_balanced_and_has_four_warmup_pairs(self):
        self.assertEqual(eri.WARMUP_PAIRS, 4)
        self.assertEqual(eri.ORDER, ((0, 1), (1, 0), (1, 0), (0, 1)))
        self.assertEqual([pair[0] for pair in eri.ORDER].count(0), 2)

    def test_each_invalid_null_condition_rejects(self):
        cases = {
            "center": [1.02] * 4,
            "order strata": [1.02, 0.98, 1.02, 0.98],
            "temporal strata": [1.02, 1.02, 0.98, 0.98],
            "individual pair": [1.051, 0.983, 0.983, 0.983],
            "null floor": [1.031, 0.969, 0.969, 1.031],
        }
        for name, values in cases.items():
            with self.subTest(name=name), self.assertRaisesRegex(RuntimeError, name):
                eri.qualify_null(values)
        with self.assertRaisesRegex(RuntimeError, "invariant floor"):
            eri.qualify_null([1.016, 0.984, 0.984, 1.016], invariant=True)

    def test_qualified_null_returns_observed_floor(self):
        self.assertAlmostEqual(eri.qualify_null([1.004, 0.997, 1.003, 0.998]), 0.004)

    def test_resume_identity_is_immutable(self):
        config = {"schema": eri.SCHEMA, "rounds": 4, "arms": {}, "workloads": {}}
        identity = eri.campaign_identity(config)
        self.assertEqual(identity, eri.campaign_identity(dict(config)))
        changed = dict(config, rounds=8)
        self.assertNotEqual(eri.campaign_identity(config), eri.campaign_identity(changed))
        with tempfile.TemporaryDirectory() as directory:
            manifest = pathlib.Path(directory) / "manifest.json"
            manifest.write_text('{"identity":"different"}')
            with self.assertRaisesRegex(RuntimeError, "different identity"):
                eri.validate_resume(manifest, identity)
            manifest.write_text('{"identity":"' + identity + '"}')
            eri.validate_resume(manifest, identity)
            manifest.unlink()
            with self.assertRaisesRegex(RuntimeError, "absent"):
                eri.validate_resume(manifest, identity)

    def test_output_mismatch_rejects(self):
        row = {"workload": "malloc", "output": "one", "phases": {"malloc": {"ok": "1"}}}
        with self.assertRaisesRegex(RuntimeError, "exact-output mismatch"):
            eri.verify_outputs([row, dict(row, output="two")])

    def test_timing_alone_is_normalized(self):
        left = eri.canonical(b"PHASE malloc us=12 ok=7\n", b"")
        right = eri.canonical(b"PHASE malloc us=99 ok=7\n", b"")
        self.assertEqual(left, right)
        self.assertNotEqual(left, eri.canonical(b"PHASE malloc us=99 ok=8\n", b""))

    def test_tree_identity_covers_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "value").write_text("one")
            first = eri.tree_digest(root)
            (root / "value").write_text("two")
            self.assertNotEqual(first, eri.tree_digest(root))

    def test_backend_receipt_is_executable_and_engine_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            engine = pathlib.Path(directory) / "engine"
            engine.write_text(
                "#!/bin/sh\n"
                "hash=$(sha256sum \"$0\" | cut -d' ' -f1)\n"
                "printf '{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\"%s\"}\\n' \"$hash\"\n"
            )
            engine.chmod(0o755)
            sha256 = eri.digest(engine)
            arm = {
                "artifacts": {"engine": {"path": str(engine), "sha256": sha256}},
                "backend_receipt": {
                    "command": [str(engine), "--backend-receipt"],
                    "backend": "retained-c",
                    "engine_sha256": sha256,
                },
            }
            self.assertEqual(eri.verify_backend_receipt("R", arm)["backend"], "retained-c")
            arm["backend_receipt"]["backend"] = "integrated-c"
            with self.assertRaisesRegex(ValueError, "does not match"):
                eri.verify_backend_receipt("R", arm)

    def test_backend_receipt_cannot_name_a_different_engine(self):
        with tempfile.TemporaryDirectory() as directory:
            engine = pathlib.Path(directory) / "engine"
            other = pathlib.Path(directory) / "other"
            engine.write_text("#!/bin/sh\nexit 0\n")
            other.write_text("#!/bin/sh\nexit 0\n")
            engine.chmod(0o755)
            other.chmod(0o755)
            sha256 = eri.digest(engine)
            arm = {
                "artifacts": {"engine": {"path": str(engine), "sha256": sha256}},
                "backend_receipt": {
                    "command": [str(other)],
                    "backend": "retained-c",
                    "engine_sha256": sha256,
                },
            }
            with self.assertRaisesRegex(ValueError, "not bound"):
                eri.verify_backend_receipt("R", arm)


if __name__ == "__main__":
    unittest.main()
