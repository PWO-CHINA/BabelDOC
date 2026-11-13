from pathlib import Path

from babeldoc.format.pdf.document_il import il_version_1 as il
from lxml import etree
from xsdata.formats.dataclass.parsers import XmlParser
from xsdata.formats.dataclass.serializers import XmlSerializer


def test_synthetic_spacing_xml_round_trip_and_schema():
    character = il.PdfCharacter(
        pdf_style=il.PdfStyle(
            font_id="F1", font_size=10.0, graphic_state=il.GraphicState()
        ),
        box=il.Box(x=10.0, y=20.0, x2=21.91216, y2=30.0),
        char_unicode=" ",
        advance=11.91216,
        synthetic_spacing=True,
    )
    xml = XmlSerializer().render(character)
    parsed = XmlParser().from_string(xml, il.PdfCharacter)
    assert parsed.synthetic_spacing is True
    assert parsed.pdf_character_id is None
    assert parsed.advance == 11.91216
    schema_dir = Path(__file__).resolve().parents[3] / "babeldoc/format/pdf/document_il"
    root = etree.fromstring(xml.encode())
    etree.XMLSchema(etree.parse(str(schema_dir / "il_version_1.xsd"))).assertValid(root)
    grammar = etree.parse(str(schema_dir / "il_version_1.rng"))
    ns = {"rng": "http://relaxng.org/ns/structure/1.0"}
    grammar.find("rng:start/rng:ref", ns).set("name", "PDFCharacter")
    etree.RelaxNG(grammar).assertValid(root)


def test_old_characters_default_to_unmarked():
    character = il.PdfCharacter(char_unicode=" ", pdf_character_id=32)
    parsed = XmlParser().from_string(XmlSerializer().render(character), il.PdfCharacter)
    assert parsed.synthetic_spacing is None
    assert parsed.pdf_character_id == 32
