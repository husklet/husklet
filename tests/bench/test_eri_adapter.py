import importlib.util
import pathlib
import subprocess
import tempfile
import unittest


ADAPTER = pathlib.Path(__file__).with_name("eri_adapter.py")
CONFIG = pathlib.Path(__file__).with_name("eri_config.py")
SPEC = importlib.util.spec_from_file_location("eri_config", CONFIG)
config = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(config)


class AdapterTests(unittest.TestCase):
    def test_adapter_selects_guest_or_wall_time(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = pathlib.Path(directory)
            engine = directory / "engine"
            engine.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' 'ignored' 'PHASE python us=7 ok=42' 'PHASE malloc us=11 ok=43'\n"
            )
            engine.chmod(0o755)
            guest = directory / "guest"
            guest.write_text("fixture")
            result = subprocess.run(
                [
                    str(ADAPTER), "--provider", "product",
                    "--engine", str(engine), "--rootfs", str(directory),
                    "--wall-phase", "python", "--", str(guest),
                ],
                text=True,
                capture_output=True,
                check=True,
            )
            self.assertEqual(result.stderr, "")
            rows = result.stdout.splitlines()
            self.assertRegex(rows[0], r"^PHASE python us=[1-9][0-9]* ok=42$")
            self.assertEqual(rows[1], "PHASE malloc us=11 ok=43")

    def test_rootfs_digest_covers_contents_and_symlink_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "file").write_text("one")
            (root / "link").symlink_to("file")
            first = config.tree_digest(root)
            (root / "file").write_text("two")
            self.assertNotEqual(first, config.tree_digest(root))
            (root / "file").write_text("one")
            (root / "link").unlink()
            (root / "link").symlink_to("missing")
            self.assertNotEqual(first, config.tree_digest(root))

    def test_config_probe_rejects_an_engine_without_backend_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            engine = pathlib.Path(directory) / "engine"
            engine.write_text("#!/bin/sh\nexit 0\n")
            engine.chmod(0o755)
            with self.assertRaisesRegex(ValueError, "backend receipt"):
                config.backend_receipt(engine, "retained-c", ("HL_EXECUTION_BACKEND=c",))


if __name__ == "__main__":
    unittest.main()
