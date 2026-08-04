import unittest

import priority


class PriorityTest(unittest.TestCase):
    def test_evidence_is_known_and_sorted(self):
        known = {"read": "supported", "clone3": "router-domain-only"}
        self.assertEqual(priority.evidence("SYS_read __NR_clone3 SYS_unknown SYS_read", known), ["clone3", "read"])

    def test_gap_precedes_supported(self):
        known = {"read": "supported", "clone3": "router-domain-only", "openat2": "missing"}
        self.assertEqual(priority.select(["read", "clone3", "openat2"], known), ("openat2", "missing"))

    def test_family_is_deterministic(self):
        self.assertEqual(priority.family("futex"), "synchronization")
        self.assertEqual(priority.family("openat2"), "filesystem")
        self.assertEqual(priority.family("clone3"), "process")

    def test_checked_outputs_current(self):
        manifest, report = priority.render(priority.analyze())
        self.assertEqual((priority.ROOT / "priority.tsv").read_text(), manifest)
        self.assertEqual((priority.ROOT / "COMPAT_PRIORITY.md").read_text(), report)


if __name__ == "__main__":
    unittest.main()
