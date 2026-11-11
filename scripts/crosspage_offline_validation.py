"""Run selected local cross-page samples with a synthetic translator."""

import argparse
import contextlib
import codecs
from collections import Counter
from datetime import datetime, timezone
import hashlib
import ipaddress
import json
import logging
import os
import re
import socket
import subprocess
import sys
import tempfile
import traceback
import unicodedata
from pathlib import Path


RETRY_INSTRUCTION = "Return only the requested JSON. Preserve each item's placeholders exactly and in order.\n\n"
PROTECTED_TEXT = re.compile(r"(<[^<>]*>|\{[^{}]*\}|(?<=\d)[eErR](?=[+-]?\d))")


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def package_digest(package):
    hasher = hashlib.sha256()
    files = sorted(path for path in package.rglob("*") if path.is_file() and path.suffix in {".py", ".pyd", ".so"})
    for path in files:
        hasher.update(path.relative_to(package).as_posix().encode("utf-8"))
        hasher.update(b"\0")
        hasher.update(bytes.fromhex(digest(path)))
    return {"sha256": hasher.hexdigest(), "file_count": len(files)}


def normalized(text):
    text = re.sub(r"<[^>]*>|\{v\d+\}", "", text)
    return re.sub(r"\W+", "", text, flags=re.UNICODE).casefold()


def synthetic_text(text, mode):
    if mode == "identity":
        return text
    if mode != "rot13":
        raise ValueError("Unsupported synthetic translator mode")
    parts = PROTECTED_TEXT.split(text)
    return "".join(part if index % 2 else codecs.encode(part, "rot_13") for index, part in enumerate(parts))


def read_local_json(path):
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, ValueError, UnicodeError):
        raise ValueError("Could not read required local JSON metadata") from None


def load_manifest(path):
    manifest = read_local_json(path)
    if not isinstance(manifest, dict) or type(manifest.get("version")) is not int or manifest["version"] != 1:
        raise ValueError("Invalid local sample manifest")
    samples = manifest.get("samples")
    if not isinstance(samples, dict) or not samples:
        raise ValueError("Invalid local sample manifest")
    for name, spec in samples.items():
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", name):
            raise ValueError("Invalid local sample manifest")
        required = {"run", "sha256", "pages", "blocks", "target"}
        if not isinstance(spec, dict) or not required <= set(spec) or set(spec) - required - {"anchor_source"}:
            raise ValueError("Invalid local sample manifest")
        run, sha256, pages, blocks, target = (spec[key] for key in ("run", "sha256", "pages", "blocks", "target"))
        if not isinstance(run, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", run):
            raise ValueError("Invalid local sample manifest")
        if not isinstance(sha256, str) or not re.fullmatch(r"[0-9a-f]{64}", sha256):
            raise ValueError("Invalid local sample manifest")
        if not isinstance(pages, list) or len(pages) != 2 or any(type(page) is not int or page < 1 for page in pages):
            raise ValueError("Invalid local sample manifest")
        if blocks is not None and (
            not isinstance(blocks, list) or len(blocks) != 2 or any(type(block) is not int or block < 0 for block in blocks)
        ):
            raise ValueError("Invalid local sample manifest")
        if type(target) is not bool:
            raise ValueError("Invalid local sample manifest")
        anchor_source = spec.get("anchor_source", "historical_tracking")
        if anchor_source not in ("historical_tracking", "selected_block_edges"):
            raise ValueError("Invalid local sample manifest")
        if anchor_source == "selected_block_edges" and (not target or blocks is None):
            raise ValueError("Invalid local sample manifest")
    return samples


def block_network():
    original_resolve = socket.getaddrinfo

    def loopback(address):
        try:
            return ipaddress.ip_address(str(address[0]).split("%", 1)[0]).is_loopback
        except (ValueError, IndexError, TypeError):
            return bool(address and str(address[0]).lower() == "localhost")

    def resolve(host, *args, **kwargs):
        if not loopback((host,)):
            raise OSError("OFFLINE_GUARD: DNS blocked")
        return original_resolve(host, *args, **kwargs)

    socket.getaddrinfo = resolve
    for method in ("connect", "connect_ex", "sendto"):
        original = getattr(socket.socket, method)

        def guarded(sock, *args, original=original):
            if not args or not loopback(args[-1]):
                raise OSError("OFFLINE_GUARD: network blocked")
            return original(sock, *args)

        setattr(socket.socket, method, guarded)


def selected_input(root, downloads, spec):
    run = root / spec["run"]
    summary_path = run / "summary.json"
    record = read_local_json(summary_path)
    metadata_hashes = {"historical_summary_sha256": digest(summary_path)}
    if not isinstance(record.get("source"), str):
        raise ValueError("Invalid source metadata")
    filename = record["source"]
    if Path(filename).name != filename:
        raise ValueError("Invalid source name in historical record")
    source = downloads / filename
    if digest(source) != spec["sha256"]:
        raise ValueError("Input hash does not match the fixed sample")
    target = None
    if spec["target"] and spec.get("anchor_source") == "selected_block_edges":
        fragments = block_fragments(source, spec["blocks"])
        target = (fragments[0][-40:], fragments[1][:40])
        if any(len(anchor) < 20 for anchor in target) or anchor_locations(source, target) != [[1, 0], [0, 1]]:
            raise ValueError("Selected block anchors are not unique on their source pages")
    elif spec["target"]:
        tracking_files = list((run / "work").glob("*/translate_tracking.json"))
        if len(tracking_files) != 1:
            raise ValueError("Expected exactly one historical tracking file")
        tracking = read_local_json(tracking_files[0])
        metadata_hashes["historical_tracking_sha256"] = digest(tracking_files[0])
        old_pair = tracking["cross_page"][0]["paragraph"]
        snapshot_path = run / "target-prompt-private.json"
        snapshot = read_local_json(snapshot_path)
        metadata_hashes["historical_prompt_snapshot_sha256"] = digest(snapshot_path)
        items = [normalized(item["input"]) for item in snapshot["items"]]
        left = normalized(old_pair[0]["pdf_unicode"])
        right = normalized(old_pair[1]["pdf_unicode"])
        right_anchor = right[:20]
        for length in range(min(60, len(left)), 11, -1):
            left_anchor = left[-length:]
            if any(left_anchor + right_anchor in item for item in items):
                target = (left_anchor, right_anchor)
                break
        if target is None or len(right_anchor) < 12:
            raise ValueError("Historical target anchors are not uniquely reproducible")
    return source, target, metadata_hashes


def output_geometry(pdf_path, expected_pages):
    import pymupdf

    with pymupdf.open(pdf_path) as document:
        pages = []
        for page in document:
            bounds = page.rect
            boxes = [block[:4] for block in page.get_text("blocks") if len(block) > 4 and block[4].strip()]
            outside = sum(
                x0 < bounds.x0 - 1 or y0 < bounds.y0 - 1 or x1 > bounds.x1 + 1 or y1 > bounds.y1 + 1
                for x0, y0, x1, y1 in boxes
            )
            pages.append({"text_blocks": len(boxes), "outside_page_bounds": outside})
        return {"page_count": len(document), "expected_page_count": expected_pages, "pages": pages}


def source_geometry(pdf_path, blocks):
    import pymupdf

    with pymupdf.open(pdf_path) as document:
        pages = [{"width": round(page.rect.width, 2), "height": round(page.rect.height, 2)} for page in document]
        regions = []
        if blocks:
            for index, block in enumerate(blocks):
                page = document[index]
                x0, y0, x1, y1 = page.get_text("blocks")[block][:4]
                regions.append({
                    "page": index + 1,
                    "pdf_rect": [round(x0, 2), round(page.rect.height - y1, 2), round(x1, 2), round(page.rect.height - y0, 2)],
                })
        return {"pages": pages, "selected_block_regions": regions}


def anchor_locations(pdf_path, target, mode="identity"):
    import pymupdf

    with pymupdf.open(pdf_path) as document:
        pages = [normalized(synthetic_text(page.get_text(), mode)) for page in document]
    return [[page.count(anchor) for page in pages] for anchor in target]


def block_fragments(source, blocks):
    import pymupdf

    if blocks is None:
        return None
    with pymupdf.open(source) as document:
        return [normalized(document[index].get_text("blocks")[block][4]) for index, block in enumerate(blocks)]


def find_split_locations(pdf_path, anchor, mode="identity"):
    import pymupdf

    locations = []
    with pymupdf.open(pdf_path) as document:
        for page_index, page in enumerate(document):
            lines = [
                normalized(synthetic_text("".join(span["text"] for span in line["spans"]), mode))
                for block in page.get_text("dict")["blocks"] if "lines" in block
                for line in block["lines"]
            ]
            for width in (1, 2, 3):
                if any(anchor in "".join(lines[index:index + width]) for index in range(len(lines))):
                    locations.append(page_index + 1)
                    break
    return locations


def character_text(value):
    if isinstance(value, dict):
        return "".join(
            str(item) if key == "char_unicode" and item is not None else character_text(item)
            for key, item in value.items()
        )
    if isinstance(value, list):
        return "".join(character_text(item) for item in value)
    return ""


def classify_failure(error):
    known_numeric_errors = {
        "跨页数值续句无法校验占位符；请人工核对。",
        "跨页数值续句无法安排成对翻译；请人工核对。",
        "跨页数值续句无法准备原页内容；请人工核对。",
        "跨页数值续句无法识别完整句子；请人工核对。",
        "跨页续句包含跨页数值，当前无法可靠翻译整句；请关闭实验选项并人工核对该处。",
        "跨页数值续句超出翻译请求长度；请人工核对。",
        "跨页数值续句未能完成整句翻译或拆分；请重试并人工核对。",
    }
    return (
        "unsafe_numeric_continuation"
        if isinstance(error, RuntimeError) and str(error) in known_numeric_errors
        else "unclassified"
    )


def classify_static_failure_message(message):
    numeric_errors = {
        "跨页数值续句无法校验占位符；请人工核对。": "placeholder_validation",
        "跨页数值续句无法安排成对翻译；请人工核对。": "pair_scheduling",
        "跨页数值续句无法准备原页内容；请人工核对。": "original_page_input",
        "跨页数值续句无法识别完整句子；请人工核对。": "sentence_extraction",
        "跨页续句包含跨页数值，当前无法可靠翻译整句；请关闭实验选项并人工核对该处。": "whole_sentence_plan_unavailable",
        "跨页数值续句超出翻译请求长度；请人工核对。": "request_length",
        "跨页数值续句未能完成整句翻译或拆分；请重试并人工核对。": "translation_or_reflow",
    }
    return numeric_errors.get(message, "unclassified")


def fallback_evidence(output, target):
    files = list((output / "work").glob("*/translate_tracking.json"))
    if len(files) != 1 or not target:
        return None
    tracking = read_local_json(files[0])
    categories = {"same_as_input": 0, "length_ratio": 0, "edit_distance": 0, "cross_page_validation": 0, "other": 0}
    target_slots = 0
    for event in tracking.get("cross_page", []):
        paragraphs = event.get("paragraph", [])
        if len(paragraphs) < 2 or not all(
            anchor in normalized(str(paragraph.get("pdf_unicode", "")))
            for anchor, paragraph in zip(target, paragraphs[:2], strict=True)
        ):
            continue
        for paragraph in paragraphs[:2]:
            target_slots += 1
            for entry in paragraph.get("llm_translate_trackers", []):
                if not entry.get("fallback_to_translate"):
                    continue
                message = str(entry.get("error_message", ""))
                if message == "Translation result is the same as input, fallback.":
                    category = "same_as_input"
                elif message.startswith("Translation result is too long or too short. Input:"):
                    category = "length_ratio"
                elif message.startswith("Translation result edit distance is too small. distance:"):
                    category = "edit_distance"
                elif "Cross-page response validation failed" in message:
                    category = "cross_page_validation"
                else:
                    category = "other"
                categories[category] += 1
    return {"target_slots": target_slots, "categories": categories}


def target_placeholder_evidence(output, capture_dir, target):
    files = list((output / "work").glob("*/translate_tracking.json"))
    if len(files) != 1 or not target:
        return None
    tracking = read_local_json(files[0])
    matches = [[], []]

    def visit(value, route):
        if isinstance(value, dict):
            if "pdf_unicode" in value:
                for index, anchor in enumerate(target):
                    if anchor in normalized(str(value.get("pdf_unicode") or "")):
                        matches[index].append((route, value))
            for child in value.values():
                visit(child, route)
        elif isinstance(value, list):
            for child in value:
                visit(child, route)

    for route in ("cross_page", "cross_column", "page"):
        visit(tracking.get(route, []), route)
    captured = None
    files = list(capture_dir.glob("**/styles.json.zst")) if capture_dir else []
    if len(files) == 1:
        import pyzstd

        captured = json.loads(pyzstd.decompress(files[0].read_bytes()))["document"]["page"]
    result = []
    for index, found in enumerate(matches):
        row = {"matching_tracking_paragraphs": len(found)}
        result.append(row)
        if len(found) != 1:
            continue
        route, paragraph = found[0]
        placeholders = paragraph.get("placeholders") or []
        kinds = [item.get("type") if item.get("type") in ("formula", "rich_text") else "other" for item in placeholders]
        edge = placeholders[-1 if index == 0 else 0] if placeholders else None
        tokens = []
        for item in placeholders:
            if item.get("type") == "formula":
                tokens.append(item.get("placeholder"))
            elif item.get("type") == "rich_text":
                tokens.extend((item.get("left_placeholder"), item.get("right_placeholder")))
            else:
                tokens.append(None)
        text = paragraph.get("input")
        mapping = None
        if isinstance(text, str) and all(isinstance(token, str) and token for token in tokens):
            counts = Counter(tokens)
            mapping = all(text.count(token) == count for token, count in counts.items())
            position = 0
            for token in tokens:
                position = text.find(token, position)
                if position < 0:
                    mapping = False
                    break
                position += len(token)
        row.update({
            "tracking_group": route,
            "tracking_has_fallback": any(item.get("fallback_to_translate") for item in paragraph.get("llm_translate_trackers", [])),
            "placeholder_count": len(kinds),
            "formula_count": kinds.count("formula"),
            "rich_text_count": kinds.count("rich_text"),
            "first_placeholder_type": kinds[0] if kinds else None,
            "last_placeholder_type": kinds[-1] if kinds else None,
            "input_tokens_match_record_count_and_order": mapping,
            "boundary_placeholder_matches_boundary_composition": None,
        })
        if captured is not None and edge and edge.get("type") == "rich_text":
            paragraphs = [item for item in captured[index].get("pdf_paragraph", []) if target[index] in normalized(str(item.get("unicode") or ""))]
            if len(paragraphs) == 1:
                compositions = paragraphs[0].get("pdf_paragraph_composition", [])
                if compositions:
                    boundary = compositions[-1 if index == 0 else 0].get("pdf_same_style_characters") or {}
                    characters = boundary.get("pdf_character") or []
                    group_text = "".join(str(character.get("char_unicode") or "") for character in characters)
                    row["boundary_placeholder_matches_boundary_composition"] = bool(characters) and edge.get("composition_chars") == group_text
    return result


def stage_evidence(capture_dir, target):
    import pyzstd

    result = {}
    for stage in ("translation", "typesetting"):
        files = list(capture_dir.glob(f"**/{stage}.json.zst"))
        if len(files) != 1:
            raise ValueError("Expected exactly one capture for each stage")
        artifact = json.loads(pyzstd.decompress(files[0].read_bytes()))
        pages = artifact["document"]["page"]
        stage_result = {
            "sha256": digest(files[0]),
            "page_numbers": [page["page_number"] for page in pages],
            "anchor_occurrences_in_paragraph_unicode": None,
            "anchor_occurrences_in_composition_characters": None,
        }
        if target:
            unicode_pages = [
                [normalized(paragraph.get("unicode") or "") for paragraph in page["pdf_paragraph"]]
                for page in pages
            ]
            character_pages = [
                [normalized(character_text(paragraph.get("pdf_paragraph_composition", [])))
                 for paragraph in page["pdf_paragraph"]]
                for page in pages
            ]
            for key, page_text in (
                ("anchor_occurrences_in_paragraph_unicode", unicode_pages),
                ("anchor_occurrences_in_composition_characters", character_pages),
            ):
                stage_result[key] = [
                    [sum(paragraph.count(anchor) for paragraph in page) for page in page_text]
                    for anchor in target
                ]
        result[stage] = stage_result
    return result


def sentence_boundary(text):
    for index, character in enumerate(text):
        if character not in ".!?。！？":
            continue
        if index == 0:
            return None
        previous = text[index - 1]
        following = text[index + 1:index + 2]
        if character == "." and previous.isalnum():
            if previous.isascii() and previous.isdigit():
                if following and following.isascii() and following.isdigit():
                    continue
                before_digits = text[:index].rstrip("0123456789")
                digits = index - len(before_digits)
                if digits < 2 or before_digits.endswith(".") or len(before_digits.split()) < 3:
                    continue
                if not following or not following.isspace() or not text[index + 1:].lstrip()[:1].isupper():
                    continue
            else:
                start = index
                while start and text[start - 1].isalpha():
                    start -= 1
                word = text[start:index]
                if len(word) <= 3 or word.lower() in {"prof", "assoc", "dept", "approx", "figs", "etc"}:
                    continue
        if not (previous.isalnum() or previous in ")]") or (following and not following.isspace()):
            continue
        return index + 1
    return None


def context_sentence_diagnostic(left, right):
    def plausible(text):
        text = text.strip()
        if sum(character.isalpha() for character in text) < 4 or len(text.split()) < 2:
            return False
        for line in text.splitlines():
            line = line.lstrip()
            if line.startswith(("•", "▪", "◦", "-", "–", "—")):
                return False
            prefix, separator, rest = line.partition(".")
            if separator and prefix and prefix.isascii() and prefix.isdigit() and rest[:1].isspace():
                return False
        return True

    def caption(text):
        lower = text.lstrip().lower()
        for prefix in ("figure", "fig.", "fig", "table"):
            if lower.startswith(prefix):
                rest = lower[len(prefix):]
                if rest[:1].isspace() and rest.lstrip()[:1].isascii() and rest.lstrip()[:1].isdigit():
                    return True
        return False

    def joinable(first, second):
        if not first or not second:
            return False
        if second[0].islower() and (first[-1].isalnum() or first[-1] in ",;"):
            return True
        left_words, right_words = first.split(), second.split()
        if first[-1].isalpha() and left_words and right_words:
            previous, word = left_words[-1], right_words[0]
            next_word = right_words[1] if len(right_words) > 1 else ""
            if (previous in {"of", "for", "with", "by", "using", "in", "to", "from"}
                and 2 <= len(word.encode("utf-8")) <= 6 and word.isascii() and word.isupper()
                and next_word[:1].islower()):
                return True
            if (previous in {"from", "in", "of"} and len(word) >= 4 and word[:1].isupper()
                and word[1:].islower() and next_word == "and"
                and len(right_words) > 2 and right_words[2][:1].islower()):
                return True
        match = re.match(r"([0-9]{1,4}) (\S.*)", second)
        if first[-1].isalnum() and match and match.group(2)[:1].islower():
            return True
        if first.endswith("-"):
            before = re.search(r"[a-z]+$", first[:-1])
            after = re.match(r"[a-z]+", second)
            return bool(before and after and len(before.group()) >= 4 and len(after.group()) >= 3)
        return False

    left, right = left.strip(), right.strip()
    offset = 0
    while (end := sentence_boundary(left[offset:])) is not None:
        offset += end
    if offset == 0:
        if not left[:1].isupper() or len(left.split()) < 5 or any(char in ".!?。！？:：" for char in left):
            return {"parts": "left_initial_fragment_rejected", "extraction": "not_attempted"}
        tail = left
    else:
        if not plausible(left[:offset]):
            return {"parts": "left_prior_fragment_rejected", "extraction": "not_attempted"}
        tail = left[offset:].strip()
    if caption(tail) or caption(right):
        return {"parts": "caption_label_rejected", "extraction": "not_attempted"}
    if not plausible(tail):
        return {"parts": "left_tail_rejected", "extraction": "not_attempted"}
    if not joinable(tail, right):
        return {"parts": "continuation_join_rejected", "extraction": "not_attempted"}
    right_end = sentence_boundary(right)
    if right_end is None:
        return {"parts": "right_sentence_boundary_missing", "extraction": "not_attempted"}
    continuation = right[:right_end]
    if not plausible(continuation):
        return {"parts": "right_fragment_rejected", "extraction": "not_attempted"}
    if continuation[-1:] not in ".!?。！？":
        return {"parts": "present", "extraction": "terminator_rejected"}
    fragments = f"{tail} {continuation[:-1]}"
    parentheses = 0
    for index, character in enumerate(fragments):
        if character == "(":
            parentheses += 1
        elif character == ")":
            parentheses -= 1
            if parentheses < 0:
                return {"parts": "present", "extraction": "parentheses_rejected"}
        elif character == ".":
            start = index
            while start and fragments[start - 1].isalpha():
                start -= 1
            if not parentheses or fragments[start:index].lower() not in {"fig", "figs"}:
                return {"parts": "present", "extraction": "internal_period_rejected"}
        elif character in "!?。！？" or unicodedata.category(character) == "Cc":
            return {"parts": "present", "extraction": "punctuation_or_control_rejected"}
    return {"parts": "present", "extraction": "parentheses_rejected" if parentheses else "available"}


def device_color_instructions_only(instructions):
    if not isinstance(instructions, str) or not instructions.strip():
        return False
    numbers = re.compile(r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)\Z")
    arities = {"g": 1, "G": 1, "rg": 3, "RG": 3, "k": 4, "K": 4}
    operands = 0
    for token in instructions.split():
        if numbers.fullmatch(token):
            operands += 1
        elif token in arities and operands == arities[token]:
            operands = 0
        else:
            return False
    return operands == 0 and instructions.split()[-1] in arities


def composition_sentence_evidence(left, right):
    def compact(text):
        return "".join(character for character in text if not character.isspace())

    def parts(paragraph):
        ranges = []
        position = 0
        for composition in paragraph.get("pdf_paragraph_composition", []):
            content = compact(character_text(composition))
            kind = "formula" if composition.get("pdf_formula") else (
                "rich" if composition.get("pdf_same_style_characters") else "other"
            )
            ranges.append((position, position + len(content), kind))
            position += len(content)
        joined = "".join(compact(character_text(composition)) for composition in paragraph.get("pdf_paragraph_composition", []))
        return ranges, joined == compact(str(paragraph.get("unicode") or ""))

    left_ranges, left_matches = parts(left)
    right_ranges, right_matches = parts(right)
    result = {"paragraph_and_compositions_match_ignoring_whitespace": [left_matches, right_matches]}
    left_text = str(left.get("unicode") or "").strip()
    right_text = str(right.get("unicode") or "").strip()
    if not left_matches or not right_matches or context_sentence_diagnostic(left_text, right_text)["parts"] != "present":
        return result
    offset = 0
    while (end := sentence_boundary(left_text[offset:])) is not None:
        offset += end
    tail = left_text[offset:].strip() if offset else left_text
    right_end = sentence_boundary(right_text)
    if right_end is None:
        return result
    left_end = len(compact(left_text))
    left_start = left_end - len(compact(tail))
    right_stop = len(compact(right_text[:right_end]))
    formula_ends = [end for _, end, kind in left_ranges if kind == "formula"]
    spans = [(left_start, left_end, left_ranges), (0, right_stop, right_ranges)]
    overlaps = lambda start, end, first, last: start < last and end > first
    result.update({
        "left_tail_after_all_formula_compositions": not formula_ends or left_start >= max(formula_ends),
        "right_sentence_and_end_in_first_rich_composition": bool(right_ranges) and right_ranges[0][2] == "rich" and right_stop <= right_ranges[0][1],
        "right_first_composition_has_content_after_sentence": bool(right_ranges) and right_stop < right_ranges[0][1],
        "right_later_compositions_have_content": any(end > start for start, end, _ in right_ranges[1:]),
        "left_sentence_tail_has_numeric": any(character.isnumeric() for character in tail),
        "right_paragraph_has_numeric": any(character.isnumeric() for character in right_text),
        "right_continuation_has_numeric": any(character.isnumeric() for character in right_text[:right_end]),
        "sentence_overlaps_formula_composition": any(
            kind == "formula" and overlaps(start, end, first, last)
            for first, last, ranges in spans for start, end, kind in ranges
        ),
        "internal_rich_composition_start_count": sum(
            kind == "rich" and first < start < last
            for first, last, ranges in spans for start, _, kind in ranges
        ),
    })
    return result


def styles_evidence(capture_dir, target):
    import pyzstd

    files = list(capture_dir.glob("**/styles.json.zst"))
    if len(files) != 1 or not target:
        return {"available": False, "file_count": len(files)}
    artifact = json.loads(pyzstd.decompress(files[0].read_bytes()))
    pages = artifact["document"]["page"]
    result = {"available": True, "sha256": digest(files[0]), "sides": []}
    evidence = []
    for index, anchor in enumerate(target):
        page = pages[index]
        matches = [paragraph for paragraph in page.get("pdf_paragraph", []) if anchor in normalized(str(paragraph.get("unicode", "")))]
        side = {"matching_paragraphs": len(matches)}
        result["sides"].append(side)
        if len(matches) != 1:
            return result
        paragraph = matches[0]
        compositions = paragraph.get("pdf_paragraph_composition", [])
        boundary = compositions[-1 if index == 0 else 0] if compositions else {}
        group = boundary.get("pdf_same_style_characters") or {}
        style = group.get("pdf_style") or {}
        characters = group.get("pdf_character") or []
        font_id = style.get("font_id")
        fonts = [font for font in page.get("pdf_font", []) if font.get("font_id") == font_id]
        xref = fonts[0].get("xref_id") if len(fonts) == 1 else None
        scope = paragraph.get("xobj_id")
        state = style.get("graphic_state")
        empty_state = state is None or (
            isinstance(state, dict) and set(state) == {"passthrough_per_char_instruction"}
            and state["passthrough_per_char_instruction"] in (None, "")
        )
        transform_fields = ("scale", "visual_roundtrip_text_render_mode", "visual_roundtrip_text_scale_x", "visual_roundtrip_text_scale_y")
        text = str(paragraph.get("unicode") or "")
        group_text = "".join(str(character.get("char_unicode") or "") for character in characters)
        side.update({
            "body_label": paragraph.get("layout_label") in ("text", "plain text", "paragraph_hybrid"),
            "paragraph_vertical": bool(paragraph.get("vertical")),
            "composition_count": len(compositions),
            "formula_composition_count": sum(bool(composition.get("pdf_formula")) for composition in compositions),
            "boundary_composition_rich": bool(group),
            "boundary_character_count": len(characters),
            "font_id_resolves_uniquely": len(fonts) == 1,
            "font_xref_positive": isinstance(xref, int) and xref > 0,
            "graphic_state_field_present": "graphic_state" in style,
            "graphic_state_empty": empty_state,
            "graphic_state_device_color_only": device_color_instructions_only(state.get("passthrough_per_char_instruction")) if isinstance(state, dict) else False,
            "scope_none_or_zero": scope in (None, 0),
            "scope_conflicts_with_page_xobject": any(obj.get("xobj_id") == scope for obj in page.get("pdf_xobject", [])),
            "all_char_scopes_match": bool(characters) and all("xobj_id" in character and character["xobj_id"] == scope for character in characters),
            "all_char_styles_match": bool(characters) and all(character.get("pdf_style") == style for character in characters),
            "all_char_vertical_safe": bool(characters) and all("vertical" in character and character["vertical"] in (None, False) for character in characters),
            "all_char_transforms_null": bool(characters) and all(all(field in character and character[field] is None for field in transform_fields) for character in characters),
        })
        evidence.append((paragraph, xref, style.get("font_size"), text, group_text, state))
    left, right = evidence
    result["boundary_font_xref_equal"] = left[1] is not None and left[1] == right[1]
    result["boundary_font_size_equal"] = left[2] is not None and left[2] == right[2]
    result["boundary_graphic_state_string_equal"] = (
        left[5].get("passthrough_per_char_instruction") == right[5].get("passthrough_per_char_instruction")
        if isinstance(left[5], dict) and isinstance(right[5], dict)
        and isinstance(left[5].get("passthrough_per_char_instruction"), str)
        and isinstance(right[5].get("passthrough_per_char_instruction"), str) else None
    )
    result["context_sentence_source_rule_replay"] = context_sentence_diagnostic(left[3], right[3])
    result["sentence_to_composition_mapping"] = composition_sentence_evidence(left[0], right[0])
    result["right_starts_with_whitespace"] = bool(right[3][:1] and right[3][0].isspace())
    result["left_last_alphanumeric"] = bool(left[3].rstrip()[-1:] and left[3].rstrip()[-1].isalnum())
    result["right_first_lowercase"] = bool(right[3].strip()[:1] and right[3].strip()[0].islower())
    result["right_first_group_prefix_matches_unicode"] = right[3].startswith(right[4]) and bool(right[4])
    result["right_anchor_in_first_group"] = target[1] in normalized(right[4])
    group_end = sentence_boundary(right[4])
    result["right_first_group_sentence_boundary_found"] = group_end is not None
    result["right_first_group_has_content_after_boundary"] = bool(right[4][group_end:].strip()) if group_end is not None else None
    end = sentence_boundary(right[3])
    result["right_sentence_boundary_found"] = end is not None
    result["right_sentence_boundary_in_first_group"] = (
        end <= len(right[4]) if end is not None and result["right_first_group_prefix_matches_unicode"] else None
    )
    result["right_following_content_after_boundary"] = bool(right[3][end:].strip()) if end is not None else None
    result["right_following_sentence_boundary_found"] = sentence_boundary(right[3][end:]) is not None if end is not None else None
    return result


def tracking_evidence(output, target, expected_fragments):
    files = list((output / "work").glob("*/translate_tracking.json"))
    if len(files) != 1:
        return {"available": False, "file_count": len(files)}
    tracking = read_local_json(files[0])
    events = tracking.get("cross_page", [])
    matched = None
    if target:
        left, right = target
        matched = sum(
            len(event.get("paragraph", [])) >= 2
            and left in normalized(str(event["paragraph"][0].get("pdf_unicode", "")))
            and right in normalized(str(event["paragraph"][1].get("pdf_unicode", "")))
            for event in events
        )
    block_matches = None
    if expected_fragments:
        block_matches = sum(
            len(event.get("paragraph", [])) >= 2
            and all(
                (candidate := normalized(str(paragraph.get("pdf_unicode", ""))))
                and len(candidate) >= 40
                and (candidate in expected or expected in candidate)
                for paragraph, expected in zip(event["paragraph"][:2], expected_fragments, strict=True)
            )
            for event in events
        )
    return {
        "available": True,
        "cross_page_events": len(events),
        "events_with_two_paragraphs": sum(len(event.get("paragraph", [])) == 2 for event in events),
        "target_pair_events": matched,
        "selected_block_pair_events": block_matches,
    }


def run_sample(source, target, output, label, spec, capture_stages=False, translator_mode="identity"):
    import pymupdf
    from babeldoc.format.pdf.high_level import translate
    from babeldoc.format.pdf.translation_config import TranslationConfig, WatermarkOutputMode
    from babeldoc.translator.translator import BaseTranslator

    counts = {
        "synthetic_calls": 0,
        "synthetic_plain_calls": 0,
        "synthetic_dedicated_calls": 0,
        "synthetic_dedicated_first_calls": 0,
        "synthetic_dedicated_retry_calls": 0,
        "synthetic_fallback_calls": 0,
        "target_fallback_calls": 0 if target else None,
        "target_fallback_after_dedicated": 0 if target else None,
        "target_dedicated_calls": 0,
        "target_dedicated_first_calls": 0,
        "target_dedicated_retry_calls": 0,
        "target_whole_input_calls": 0,
        "target_context_only_calls": 0,
        "target_response_items": 0,
        "target_response_items_unchanged": 0,
    }

    class SyntheticTranslator(BaseTranslator):
        def __init__(self):
            self.lang_in = "en"
            self.lang_out = "zh"
            self.translate_call_count = 0
            self.translate_cache_call_count = 0

        def do_translate(self, text, rate_limit_params=None):
            counts["synthetic_plain_calls"] += 1
            return synthetic_text(text, translator_mode)

        def do_llm_translate(self, text, rate_limit_params=None):
            return None if text is None else self.llm_translate(text, rate_limit_params=rate_limit_params)

        def llm_translate(self, prompt, ignore_cache=False, rate_limit_params=None):
            counts["synthetic_calls"] += 1
            marker = "## Here is the input:"
            dedicated = "## Cross-page continuation" in prompt
            retry = dedicated and prompt.startswith(RETRY_INSTRUCTION)
            counts["synthetic_dedicated_calls"] += bool(dedicated)
            counts["synthetic_dedicated_first_calls"] += dedicated and not retry
            counts["synthetic_dedicated_retry_calls"] += retry
            if marker not in prompt:
                counts["synthetic_fallback_calls"] += 1
                if target and any(anchor in normalized(prompt) for anchor in target):
                    counts["target_fallback_calls"] += 1
                    if counts["target_dedicated_calls"]:
                        counts["target_fallback_after_dedicated"] += 1
                fallback = "Now translate the following text:"
                if fallback not in prompt:
                    raise ValueError("Unsupported synthetic fallback prompt")
                return synthetic_text(prompt.rsplit(fallback, 1)[1].strip(), translator_mode)
            items = json.loads(prompt.rsplit(marker, 1)[1].strip())
            outputs = [{"id": item["id"], "output": synthetic_text(item["input"], translator_mode)} for item in items]
            if dedicated and target:
                left, right = target
                content = normalized(prompt)
                whole = any(left + right in normalized(item.get("input", "")) for item in items)
                if left in content and right in content:
                    counts["target_dedicated_calls"] += 1
                    counts["target_dedicated_first_calls"] += not retry
                    counts["target_dedicated_retry_calls"] += retry
                    counts["target_whole_input_calls"] += bool(whole)
                    counts["target_context_only_calls"] += not whole
                    counts["target_response_items"] += len(items)
                    counts["target_response_items_unchanged"] += sum(
                        item["input"] == output["output"] for item, output in zip(items, outputs, strict=True)
                    )
            return json.dumps(outputs)

        def __str__(self):
            return f"offline-synthetic-{translator_mode}"

    with pymupdf.open(source) as document:
        input_pages = len(document)
    result = {
        "sample": label,
        "source_sha256": digest(source),
        "input_pages": input_pages,
        "provider_requests": 0,
        "translator": f"synthetic_{translator_mode}",
        "target_selection": f"{spec.get('anchor_source', 'historical_tracking')}_anchors" if target else "unresolved",
        "writeback": ("inverse_rot13_extraction_anchor_locations_only" if translator_mode == "rot13" else "anchor_locations_only") if target else "not_verified",
        "layout": "not_verified",
        "finished": False,
    }
    if result["source_sha256"] != spec["sha256"]:
        raise ValueError("Input hash changed after preflight")
    if target:
        result["source_anchor_occurrences_by_page"] = anchor_locations(source, target)
    selected_blocks = spec["blocks"]
    expected_fragments = block_fragments(source, selected_blocks)
    result["selected_block_indexes"] = selected_blocks
    result["source_geometry"] = source_geometry(source, selected_blocks)
    output.mkdir(parents=True, exist_ok=True)
    try:
        config = TranslationConfig(
            translator=SyntheticTranslator(), input_file=source, lang_in="en", lang_out="zh",
            doc_layout_model=None, output_dir=output, working_dir=output / "work", debug=False,
            no_dual=True, auto_extract_glossary=False, pool_max_workers=1, qps=20,
            remove_non_formula_lines=True, watermark_output_mode=WatermarkOutputMode.NoWatermark,
            use_rich_pbar=False, experimental_cross_page_context=True,
            capture_stages=capture_stages, capture_dir=output / "capture",
        )
        with open(os.devnull, "w", encoding="utf-8") as discard, contextlib.redirect_stdout(discard), contextlib.redirect_stderr(discard):
            translation = translate(config)
        result["output_geometry"] = output_geometry(Path(translation.mono_pdf_path), input_pages)
        result["output_geometry"]["sha256"] = digest(Path(translation.mono_pdf_path))
        result["recognition_tracking"] = tracking_evidence(output, target, expected_fragments)
        result["target_tracking_fallback_evidence"] = fallback_evidence(output, target)
        if capture_stages:
            result["stage_capture"] = stage_evidence(output / "capture", target)
        if target:
            result["output_anchor_occurrences_by_page"] = anchor_locations(Path(translation.mono_pdf_path), target, translator_mode)
            result["output_anchor_page_line_windows"] = [
                find_split_locations(Path(translation.mono_pdf_path), anchor, translator_mode) for anchor in target
            ]
        result["layout"] = "bounds_only_not_visual_review"
        result["finished"] = True
    except Exception as error:
        result["error_type"] = type(error).__name__
        result["known_failure_class"] = classify_failure(error)
        result["known_failure_stage"] = classify_static_failure_message(str(error))
        result["error_frames"] = [
            {"file": Path(frame.filename).name, "line": frame.lineno, "function": frame.name}
            for frame in traceback.extract_tb(error.__traceback__)
        ]
    if capture_stages:
        try:
            result["styles_capture"] = styles_evidence(output / "capture", target)
        except (OSError, ValueError, KeyError, IndexError, TypeError):
            result["styles_capture"] = {"available": False, "classification": "unavailable_or_invalid"}
    try:
        result["target_tracking_placeholder_evidence"] = target_placeholder_evidence(
            output, output / "capture" if capture_stages else None, target
        )
    except (OSError, ValueError, KeyError, IndexError, TypeError):
        result["target_tracking_placeholder_evidence"] = None
    result.update(counts)
    if not target:
        for key in ("target_dedicated_calls", "target_dedicated_first_calls", "target_dedicated_retry_calls", "target_whole_input_calls", "target_context_only_calls", "target_response_items", "target_response_items_unchanged"):
            result[key] = None
    result["source_unchanged"] = digest(source) == result["source_sha256"]
    result["completed_at_utc"] = datetime.now(timezone.utc).isoformat()
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--suite-root", type=Path, required=True)
    parser.add_argument("--downloads", type=Path, required=True)
    parser.add_argument("--package-root", type=Path, required=True)
    parser.add_argument("--native-sha256", required=True)
    parser.add_argument("--package-code-sha256")
    parser.add_argument("--source-checkout", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument("--capture-stages", action="store_true")
    parser.add_argument("--translator-mode", choices=("identity", "rot13"), default="identity")
    parser.add_argument("--sample", action="append", required=True)
    args = parser.parse_args()
    samples = load_manifest(args.manifest.resolve(strict=True))
    root = args.suite_root.resolve(strict=True)
    downloads = args.downloads.resolve(strict=True)
    package_root = args.package_root.resolve(strict=True)
    if len(set(args.sample)) != len(args.sample):
        parser.error("Duplicate sample")
    if any(name not in samples for name in args.sample):
        parser.error("Unknown sample name")
    if bool(args.source_checkout) != bool(args.source_commit):
        parser.error("Source checkout and commit must be supplied together")
    source_identity = None
    checkout = None
    if args.source_checkout:
        checkout = args.source_checkout.resolve(strict=True)
        commit = subprocess.check_output(["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
        status = subprocess.check_output(["git", "-C", str(checkout), "status", "--porcelain"], text=True)
        if commit != args.source_commit.lower() or status:
            raise SystemExit("Source checkout must be clean and match the full requested commit")
        source_identity = {"checkout": str(checkout), "commit": commit, "clean": True}
    for key in list(os.environ):
        if any(part in key.upper() for part in ("KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "PROXY")):
            os.environ.pop(key, None)
    block_network()
    logging.disable(logging.CRITICAL)
    import babeldoc
    import babeldoc._rust_il as native

    module_path = Path(babeldoc.__file__).resolve()
    native_path = Path(native.__file__).resolve()
    native_hash = digest(native_path)
    if not module_path.is_relative_to(package_root) or not native_path.is_relative_to(package_root):
        raise SystemExit("Package and native must both come from the requested package root")
    if native_hash != args.native_sha256.lower():
        raise SystemExit("Native SHA-256 does not match")
    capture_enabled = bool(native.stage_capture_enabled())
    if args.capture_stages and not capture_enabled:
        raise SystemExit("The requested native was built without stage capture")
    package_hash = package_digest(module_path.parent)
    if args.package_code_sha256 and package_hash["sha256"] != args.package_code_sha256.lower():
        raise SystemExit("Package code SHA-256 does not match")
    checks = {"blocked": 0, "unexpectedly_allowed": 0}

    def connect_external():
        with socket.socket() as guarded_socket:
            guarded_socket.connect(("192.0.2.1", 443))

    for call in (
        lambda: socket.getaddrinfo("offline-guard.invalid", 443),
        connect_external,
    ):
        try:
            call()
        except OSError:
            checks["blocked"] += 1
        else:
            checks["unexpectedly_allowed"] += 1
    if checks["blocked"] != 2:
        raise SystemExit("Python socket guard self-test failed")
    report = {
        "runner_sha256": digest(Path(__file__).resolve()),
        "native_path": str(native_path),
        "native_sha256": native_hash,
        "package_path": str(module_path),
        "package_code": package_hash,
        "source_checkout": source_identity,
        "source_native_relationship": "requires_build_provenance",
        "stage_capture_enabled": capture_enabled,
        "stage_capture_requested": args.capture_stages,
        "python": sys.executable,
        "python_socket_guard": True,
        "python_socket_guard_self_test": checks,
        "provider_requests": 0,
        "samples": [],
    }
    all_inputs = [selected_input(root, downloads, samples[name]) for name in args.sample]

    def verify_identity():
        if digest(native_path) != native_hash or package_digest(module_path.parent) != package_hash:
            raise RuntimeError("Installed package changed during validation")
        if checkout:
            current = subprocess.check_output(["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
            dirty = subprocess.check_output(["git", "-C", str(checkout), "status", "--porcelain"], text=True)
            if current != args.source_commit.lower() or dirty:
                raise RuntimeError("Source checkout changed during validation")

    with tempfile.TemporaryDirectory(prefix="crosspage-offline-") as directory:
        for name, (source, target, metadata_hashes) in zip(args.sample, all_inputs, strict=True):
            verify_identity()
            row = run_sample(source, target, Path(directory) / name, name, samples[name], args.capture_stages, args.translator_mode)
            verify_identity()
            row["original_pages"] = samples[name]["pages"]
            row["historical_metadata_hashes"] = metadata_hashes
            report["samples"].append(row)
    print(json.dumps(report, ensure_ascii=True, indent=2))
    if any(not row["finished"] or not row["source_unchanged"] for row in report["samples"]):
        raise SystemExit(1)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(json.dumps({"runner_error_type": type(error).__name__}), file=sys.stderr)
        raise SystemExit(1) from None
