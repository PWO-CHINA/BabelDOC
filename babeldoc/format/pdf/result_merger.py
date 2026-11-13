import logging
from pathlib import Path

from pymupdf import Document

from babeldoc.format.pdf.document_il.backend.pdf_creater import PDFCreater
from babeldoc.format.pdf.translation_config import TranslateResult
from babeldoc.format.pdf.translation_config import TranslationConfig
from babeldoc.format.pdf.translation_config import WatermarkOutputMode

logger = logging.getLogger(__name__)


class ResultMerger:
    """Handles merging of split translation results"""

    def __init__(self, translation_config: TranslationConfig):
        self.config = translation_config

    def merge_results(
        self, results: dict[int, TranslateResult | None]
    ) -> TranslateResult:
        """Merge multiple translation results into one"""
        if not results:
            raise ValueError("No results to merge")

        basename = Path(self.config.input_file).stem
        debug_suffix = ".debug" if self.config.debug else ""

        mono_file_name = f"{basename}{debug_suffix}.{self.config.lang_out}.mono.pdf"
        dual_file_name = f"{basename}{debug_suffix}.{self.config.lang_out}.dual.pdf"

        debug_suffix += ".no_watermark"

        mono_file_name_no_watermark = (
            f"{basename}{debug_suffix}.{self.config.lang_out}.mono.pdf"
        )
        dual_file_name_no_watermark = (
            f"{basename}{debug_suffix}.{self.config.lang_out}.dual.pdf"
        )
        results = {k: v for k, v in results.items() if v is not None}
        if not results:
            raise ValueError("No results to merge")
        sorted_results = dict(sorted(results.items()))

        # Initialize paths for merged files
        merged_mono_path = None
        merged_dual_path = None
        merged_no_watermark_mono_path = None
        merged_no_watermark_dual_path = None
        separate_outputs = (
            getattr(self.config, "watermark_output_mode", None)
            == WatermarkOutputMode.Both
            and not getattr(self.config, "only_parse_generate_pdf", False)
        ) or any(
            r.dual_pdf_path != r.no_watermark_dual_pdf_path
            or r.mono_pdf_path != r.no_watermark_mono_pdf_path
            for r in results.values()
        )

        def merge_output(attribute, output_name, tag):
            paths = []
            for index, result in sorted_results.items():
                path = getattr(result, attribute)
                if not path:
                    raise OSError(f"missing {attribute} for part {index}")
                paths.append(path)
            merged_path = self._merge_pdfs(paths, output_name, tag=tag)
            if not merged_path:
                raise OSError(f"failed to merge {attribute}")
            return merged_path

        if not self.config.no_mono:
            merged_mono_path = merge_output(
                "mono_pdf_path", mono_file_name, "merged_mono"
            )
            if separate_outputs:
                merged_no_watermark_mono_path = merge_output(
                    "no_watermark_mono_pdf_path",
                    mono_file_name_no_watermark,
                    "merged_no_watermark_mono",
                )
            else:
                merged_no_watermark_mono_path = merged_mono_path

        if not self.config.no_dual:
            merged_dual_path = merge_output(
                "dual_pdf_path", dual_file_name, "merged_dual"
            )
            if separate_outputs:
                merged_no_watermark_dual_path = merge_output(
                    "no_watermark_dual_pdf_path",
                    dual_file_name_no_watermark,
                    "merged_no_watermark_dual",
                )
            else:
                merged_no_watermark_dual_path = merged_dual_path

        auto_extracted_glossary_path = None
        if (
            self.config.save_auto_extracted_glossary
            and self.config.shared_context_cross_split_part.auto_extracted_glossary
        ):
            auto_extracted_glossary_path = self.config.get_output_file_path(
                f"{basename}{debug_suffix}.{self.config.lang_out}.glossary.csv"
            )
            with auto_extracted_glossary_path.open("w", encoding="utf-8-sig") as f:
                logger.info(
                    f"save auto extracted glossary to {auto_extracted_glossary_path}"
                )
                f.write(
                    self.config.shared_context_cross_split_part.auto_extracted_glossary.to_csv()
                )

        # Create merged result
        merged_result = TranslateResult(
            mono_pdf_path=merged_mono_path,
            dual_pdf_path=merged_dual_path,
            auto_extracted_glossary_path=auto_extracted_glossary_path,
        )
        merged_result.no_watermark_mono_pdf_path = merged_no_watermark_mono_path
        merged_result.no_watermark_dual_pdf_path = merged_no_watermark_dual_path

        # Calculate total time
        total_time = sum(
            r.total_seconds for r in results.values() if hasattr(r, "total_seconds")
        )
        merged_result.total_seconds = total_time

        return merged_result

    def _merge_pdfs(
        self, pdf_paths: list[str | Path], output_name: str, tag: str
    ) -> Path:
        """Merge multiple PDFs into one"""
        if not pdf_paths:
            return None

        output_path = self.config.get_output_file_path(output_name)
        merged_doc = Document()

        for pdf_path in pdf_paths:
            doc = Document(str(pdf_path))
            merged_doc.insert_pdf(doc)

        merged_doc = PDFCreater.subset_fonts_in_subprocess(
            merged_doc, self.config, tag=tag
        )
        PDFCreater.save_pdf_with_timeout(
            merged_doc, str(output_path), translation_config=self.config
        )

        return output_path
