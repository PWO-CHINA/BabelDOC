import ast
import inspect
from pathlib import Path

from babeldoc.format.pdf.translation_config import SharedContextCrossSplitPart
from babeldoc.format.pdf.translation_config import TranslationConfig
from babeldoc.glossary import Glossary
from babeldoc.glossary import GlossaryEntry

ROOT = Path(__file__).resolve().parents[3]


def test_automatic_extraction_and_boundary_experiment_default_off():
    params = inspect.signature(TranslationConfig).parameters
    assert params["auto_extract_glossary"].default is False
    assert params["experimental_cross_page_context"].default is False


def test_user_glossary_survives_auto_and_conflicts_are_not_voted():
    user = Glossary("user", [GlossaryEntry("cell", "细胞")])
    shared = SharedContextCrossSplitPart()
    shared.initialize_glossaries([user])
    shared.add_raw_extracted_term_pair("Cell", "细胞")
    shared.add_raw_extracted_term_pair("cell", "细胞")
    shared.add_raw_extracted_term_pair("CELL", "牢房")
    shared.add_raw_extracted_term_pair("Nature", "《自然》")
    shared.finalize_auto_extracted_glossary()
    assert [(entry.source, entry.target) for entry in shared.auto_extracted_glossary.entries] == [("Nature", "《自然》")]
    assert shared.get_glossaries_for_translation(True) == [user]
    assert shared.get_glossaries_for_translation(False) == [user]


def test_cli_still_explicitly_supplies_its_own_default():
    tree = ast.parse((ROOT / "babeldoc/main.py").read_text(encoding="utf-8"))
    calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
             and any(isinstance(arg, ast.Constant) and arg.value == "--no-auto-extract-glossary" for arg in node.args)]
    assert len(calls) == 1
    assert any(keyword.arg == "default" and ast.literal_eval(keyword.value) is True for keyword in calls[0].keywords)
