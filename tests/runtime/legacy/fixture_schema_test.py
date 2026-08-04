import unittest

import fixture_schema

class FixtureSchemaTest(unittest.TestCase):
    def test_self_contained_process_does_not_require_an_external_service(self):
        record = {
            "suite": "process", "case": "owned-fork", "isa": "aarch64",
            "dependencies": "linux-libc,fork,self-contained-process", "defines": "-",
            "env": "-", "stdout": "tests/compat/process/expected/owned.out",
        }
        classified = fixture_schema.classify(record, "process")
        self.assertEqual(classified["fixture"], "executable")
        self.assertEqual(classified["multi_process"], "true")

    def test_concrete_schemas(self):
        base = {"suite": "filesystem", "case": "pc-libmap", "isa": "aarch64", "defines": "argv:/tmp/hl_pclib_blob.bin", "env": "-", "dependencies": "linux-libc", "stdout": "expected.out"}
        row = fixture_schema.classify(base)
        self.assertEqual(row["fixture"], "side-file")
        self.assertEqual(row["side_files"], "pclib_blob_arm.bin")
        self.assertEqual(row["arguments"], "/tmp/hl_pclib_blob.bin")

    def test_checked_outputs_current(self):
        manifest, report = fixture_schema.render(fixture_schema.analyze())
        self.assertEqual((fixture_schema.ROOT / "fixture-schema.tsv").read_text(), manifest)
        self.assertEqual((fixture_schema.ROOT / "FIXTURE_SCHEMA.md").read_text(), report)

    def test_explicit_network_modes_are_ordinary_executables(self):
        base = {"suite": "process", "case": "inet", "isa": "aarch64", "defines": "-", "dependencies": "linux-libc", "stdout": "expected.out"}
        for environment in ("HL_NET_HOST=1", "HL_NET_ISOLATE=1"):
            row = fixture_schema.classify({**base, "env": environment})
            self.assertEqual(row["fixture"], "executable")
            self.assertEqual(row["network_setup"], "false")

    def test_network_and_device_capabilities_are_separate(self):
        base = {"suite": "network", "case": "loopback", "isa": "aarch64",
                "defines": "-", "env": "-", "stdout": "expected.out"}
        network = fixture_schema.classify({**base, "dependencies": "linux-libc,network"})
        device = fixture_schema.classify({**base, "dependencies": "linux-libc,pty"})
        both = fixture_schema.classify({**base, "dependencies": "linux-libc,pty,network"})
        self.assertEqual(network["fixture"], "network-sandbox")
        self.assertEqual(device["fixture"], "special-device")
        self.assertEqual(both["fixture"], "special-device")
        self.assertEqual((network["special_device"], network["network_setup"]), ("false", "true"))
        self.assertEqual((device["special_device"], device["network_setup"]), ("true", "false"))


if __name__ == "__main__":
    unittest.main()
