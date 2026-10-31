import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("bridge", Path(__file__).parents[1] / "bridge.py")
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class BoundaryTests(unittest.TestCase):
    def request(self):
        return {"protocol_version": 1, "prompt": "synthetic test", "max_turns": 3,
                "model": "test-model", "mcp_servers": {"rshell": {"type": "stdio", "command": "test"}}}

    def test_sdk_options_keep_terminal_authority_in_rust(self):
        options = bridge.make_options(self.request())
        self.assertEqual(options.tools, [])
        self.assertEqual(options.allowed_tools, bridge.TOOLS)
        self.assertEqual(options.permission_mode, "dontAsk")
        self.assertTrue(options.strict_mcp_config)
        self.assertEqual(options.setting_sources, [])
        self.assertEqual(options.model, "test-model")
        self.assertEqual(options.max_turns, 3)
        self.assertTrue(Path(options.cli_path).is_file())
        self.assertIn("_bundled", str(options.cli_path))

    def test_invalid_protocol_scope_and_capacity_fail_closed(self):
        for key, value in [("protocol_version", 2), ("max_turns", 0), ("max_turns", True),
                           ("prompt", "x" * 160001), ("mcp_servers", {"other": {}})]:
            request = self.request()
            request[key] = value
            with self.assertRaises(ValueError):
                bridge.make_options(request)


if __name__ == "__main__":
    unittest.main()
