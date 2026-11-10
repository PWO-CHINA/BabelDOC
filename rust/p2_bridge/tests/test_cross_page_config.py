import ast
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


class CrossPageConfigContractTest(unittest.TestCase):
    def test_ordinary_bridge_uses_the_public_default_off_setting(self):
        config = ast.parse(
            (ROOT / "babeldoc/format/pdf/translation_config.py").read_text(encoding="utf-8")
        )
        config_class = next(
            node for node in config.body
            if isinstance(node, ast.ClassDef) and node.name == "TranslationConfig"
        )
        initializer = next(
            node for node in config_class.body
            if isinstance(node, ast.FunctionDef) and node.name == "__init__"
        )
        arguments = initializer.args.args
        defaults = dict(zip((arg.arg for arg in arguments[-len(initializer.args.defaults):]), initializer.args.defaults, strict=True))
        self.assertIs(ast.literal_eval(defaults["experimental_cross_page_context"]), False)

        bridge = (ROOT / "rust/p2_bridge/src/ordinary_translation.rs").read_text(encoding="utf-8")
        gate = re.search(
            r'let cross_page_experiment = config\s*\.getattr\("([^"]+)"\)'
            r'.*?\.unwrap_or\(false\);\s*if cross_page_experiment\s*\{'
            r'.*?return Err\(.*?ordinary translators are not supported',
            bridge,
            re.DOTALL,
        )
        self.assertIsNotNone(gate, "ordinary bridge must reject the default-off experiment")
        self.assertEqual(gate.group(1), "experimental_cross_page_context")
        self.assertLess(gate.start(), bridge.index('let _native_font_mapper', gate.start()))


if __name__ == "__main__":
    unittest.main()
