"""Pure offline witness oracles. No Git, Cargo, actual platform check or CLI."""
import unittest
from unittest.mock import patch

import witness


class WitnessTests(unittest.TestCase):
    def test_every_required_native_step_must_really_succeed(self):
        for label in witness.RUNNERS:
            names = list(witness.COMMON) + (["linux_tools", "native_linux"] if label.startswith("ubuntu-") else ["native_macos"])
            complete = {name: {"outcome": "success", "conclusion": "success"} for name in names}
            self.assertEqual(witness.outcomes(label, complete)["status"], "passed")
            for changed in names:
                for state in ("skipped", "failure", "cancelled"):
                    incomplete = {**complete, changed: {"outcome": state, "conclusion": "success"}}
                    self.assertEqual(witness.outcomes(label, incomplete)["unsatisfied_steps"], [changed])
                incomplete = {key: item for key, item in complete.items() if key != changed}
                self.assertEqual(witness.outcomes(label, incomplete)["status"], "failed")

    def test_source_identity_requires_the_selected_complete_clean_commit(self):
        with patch("witness.command", side_effect=["a" * 40, ""]) as commands:
            self.assertTrue(witness.source("a" * 40)["worktree_clean"])
            self.assertEqual(commands.call_count, 2)
        for expected, actual, dirty in (("a" * 41, "a" * 41, ""), ("a" * 40, "b" * 40, ""), ("a" * 40, "a" * 40, " M tracked")):
            with patch("witness.command", side_effect=[actual, dirty]):
                with self.assertRaises(AssertionError):
                    witness.source(expected)

    def test_platform_labels_cannot_substitute_another_native_baseline(self):
        with patch("witness.platform.system", return_value="Linux"), patch("witness.platform.machine", return_value="x86_64"), \
             patch("witness.os_release", return_value={"ID": "ubuntu", "VERSION_ID": "22.04"}), \
             patch("witness.os.confstr", return_value="glibc 2.35"):
            self.assertEqual(witness.platform_witness("ubuntu-22.04")["machine"], "x86_64")
            with self.assertRaisesRegex(AssertionError, "architecture"):
                witness.platform_witness("ubuntu-22.04-arm")
            with patch("witness.os_release", return_value={"ID": "ubuntu", "VERSION_ID": "24.04"}):
                with self.assertRaisesRegex(AssertionError, "baseline"):
                    witness.platform_witness("ubuntu-22.04")


if __name__ == "__main__":
    unittest.main()
