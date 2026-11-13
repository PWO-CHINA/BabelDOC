use babeldoc_font_core::{
    FontFace,
    glyph_bounds_evidence::{Bounds, BoundsPen, Failure, OutlineObservation},
};
use skrifa::outline::OutlinePen;

#[test]
fn control_points_enclose_quadratic_and_cubic_extrema() {
    let mut pen = BoundsPen::new(6);
    pen.move_to(-2.0, 0.0);
    pen.quad_to(4.0, 8.0, 2.0, 0.0);
    pen.curve_to(-6.0, -9.0, 8.0, 12.0, 1.0, -1.0);
    let OutlineObservation::Bounds(bounds) = pen.finish().unwrap() else {
        panic!("missing bounds")
    };
    assert_eq!(
        bounds,
        Bounds {
            x_min: -6.0,
            y_min: -9.0,
            x_max: 8.0,
            y_max: 12.0
        }
    );
    for step in 0..=1000 {
        let t = step as f32 / 1000.0;
        let s = 1.0 - t;
        let quadratic = (
            s * s * (-2.0) + 2.0 * s * t * 4.0 + t * t * 2.0,
            2.0 * s * t * 8.0,
        );
        let cubic = (
            s * s * s * 2.0 + 3.0 * s * s * t * (-6.0) + 3.0 * s * t * t * 8.0 + t * t * t,
            3.0 * s * s * t * (-9.0) + 3.0 * s * t * t * 12.0 - t * t * t,
        );
        for (x, y) in [quadratic, cubic] {
            assert!(
                x >= bounds.x_min && x <= bounds.x_max && y >= bounds.y_min && y <= bounds.y_max
            );
        }
    }
}

#[test]
fn pen_rejects_nonfinite_and_limit_and_reports_empty() {
    assert_eq!(
        BoundsPen::new(1).finish(),
        Ok(OutlineObservation::NoOutline)
    );
    let mut pen = BoundsPen::new(2);
    pen.move_to(f32::NAN, 0.0);
    assert_eq!(pen.finish(), Err(Failure::NonFiniteCoordinate));
    let mut pen = BoundsPen::new(1);
    pen.quad_to(0.0, 0.0, 1.0, 1.0);
    assert_eq!(pen.finish(), Err(Failure::LimitExceeded));
}

#[test]
fn noto_simple_outline_empty_missing_and_composite() {
    let path = std::env::var_os("BABELDOC_TEST_FONT")
        .expect("set BABELDOC_TEST_FONT to licensed NotoSans-Regular.ttf");
    let face = FontFace::load("glyph-bounds-test", path).unwrap();
    let glyph = face.has_glyph('g' as u32);
    assert_ne!(glyph, 0);
    let evidence = face.glyph_bounds_evidence(glyph).unwrap();
    assert_eq!(evidence.glyph_id, glyph);
    assert!(evidence.units_per_em > 0);
    let OutlineObservation::Bounds(bounds) = evidence.observation else {
        panic!("missing outline")
    };
    assert!(bounds.x_min < bounds.x_max && bounds.y_min < 0.0 && bounds.y_max > 0.0);
    let space = face.has_glyph(' ' as u32);
    assert_ne!(space, 0);
    assert_eq!(
        face.glyph_bounds_evidence(space).unwrap().observation,
        OutlineObservation::NoOutline
    );
    assert_eq!(
        face.glyph_bounds_evidence(face.glyph_count()),
        Err(Failure::MissingGlyph)
    );
    assert_eq!(face.has_glyph(0x10ffff), 0);
    let notdef = face.glyph_bounds_evidence(0).unwrap();
    assert_eq!(notdef.glyph_id, 0);
    assert!(matches!(notdef.observation, OutlineObservation::Bounds(_)));
    let composite = face.has_glyph('é' as u32);
    assert_ne!(composite, 0);
    assert_eq!(
        face.glyph_bounds_evidence(composite),
        Err(Failure::UnsupportedComposite)
    );
}

#[test]
fn truncated_loca_is_rejected_for_both_formats_even_for_empty_glyph() {
    let path = std::env::var_os("BABELDOC_TEST_FONT").unwrap();
    let original = std::fs::read(&path).unwrap();
    let face = FontFace::load("loca-original", path).unwrap();
    let glyph = face.has_glyph(' ' as u32);
    assert!(glyph > 0);
    assert_eq!(
        face.glyph_bounds_evidence(glyph).unwrap().observation,
        OutlineObservation::NoOutline
    );
    let count = u16::from_be_bytes(original[4..6].try_into().unwrap()) as usize;
    let record = |tag: &[u8; 4]| {
        (0..count)
            .map(|index| 12 + index * 16)
            .find(|&index| &original[index..index + 4] == tag)
            .unwrap()
    };
    let offset = |entry: usize| {
        u32::from_be_bytes(original[entry + 8..entry + 12].try_into().unwrap()) as usize
    };
    let loca_record = record(b"loca");
    let loca = offset(loca_record);
    let head = offset(record(b"head"));
    let original_format = u16::from_be_bytes(original[head + 50..head + 52].try_into().unwrap());
    let values: Vec<u32> = (0..=glyph as usize + 1)
        .map(|index| match original_format {
            0 => {
                2 * u32::from(u16::from_be_bytes(
                    original[loca + 2 * index..loca + 2 * index + 2]
                        .try_into()
                        .unwrap(),
                ))
            }
            1 => u32::from_be_bytes(
                original[loca + 4 * index..loca + 4 * index + 4]
                    .try_into()
                    .unwrap(),
            ),
            _ => panic!("invalid loca format"),
        })
        .collect();
    assert_eq!(values[glyph as usize], values[glyph as usize + 1]);
    let dir = std::env::var_os("GLYPH_BOUNDS_EVIDENCE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("glyph-bounds-evidence"));
    std::fs::create_dir_all(&dir).unwrap();
    let run_id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut results = Vec::new();
    for format in 0..=1u16 {
        let mut truncated = original.clone();
        let stride = if format == 0 { 2 } else { 4 };
        truncated[head + 50..head + 52].copy_from_slice(&format.to_be_bytes());
        for (index, value) in values.iter().copied().enumerate() {
            if format == 0 {
                assert_eq!(value % 2, 0);
                truncated[loca + 2 * index..loca + 2 * index + 2]
                    .copy_from_slice(&u16::try_from(value / 2).unwrap().to_be_bytes());
            } else {
                truncated[loca + 4 * index..loca + 4 * index + 4]
                    .copy_from_slice(&value.to_be_bytes());
            }
        }
        let length = u32::try_from(values.len() * stride).unwrap();
        assert!((length as usize) < (face.glyph_count() as usize + 1) * stride);
        truncated[loca_record + 12..loca_record + 16].copy_from_slice(&length.to_be_bytes());
        let fixture = dir.join(format!("{run_id}-truncated-loca-{format}.ttf"));
        use std::io::Write;
        std::fs::File::create_new(&fixture)
            .unwrap()
            .write_all(&truncated)
            .unwrap();
        let truncated_face = FontFace::load("truncated-loca", &fixture).unwrap();
        assert_eq!(truncated_face.glyph_count(), face.glyph_count());
        results.push((format, fixture, truncated_face.glyph_bounds_evidence(glyph)));
    }
    for (format, fixture, result) in results {
        assert_eq!(
            result,
            Err(Failure::Parse),
            "format {format}, fixture {}",
            fixture.display()
        );
    }
}

#[test]
fn fonttools_independent_control_points_are_contained() {
    let path = std::env::var_os("BABELDOC_TEST_FONT")
        .expect("set BABELDOC_TEST_FONT to NotoSans-Regular.ttf");
    let python = std::env::var_os("GLYPH_BOUNDS_FONTTOOLS_PYTHON")
        .expect("set GLYPH_BOUNDS_FONTTOOLS_PYTHON to a Python with fontTools installed");
    let script = r#"
import hashlib, sys, fontTools
from fontTools.ttLib import TTFont
path = sys.argv[1]
with open(path, 'rb') as stream:
    print('fontTools', fontTools.__version__, 'sha256', hashlib.sha256(stream.read()).hexdigest())
font = TTFont(path, lazy=False)
cmap, glyf = font.getBestCmap(), font['glyf']
for char in 'gSj':
    name = cmap[ord(char)]
    glyph = glyf[name]
    if glyph.isComposite():
        raise ValueError('test requires simple glyph: ' + name)
    points, ends, flags = glyph.getCoordinates(glyf)
    if not points or not ends:
        raise ValueError('test requires nonempty outline: ' + name)
    offcurve = sum((flag & 1) == 0 for flag in flags)
    print('glyph', ord(char), font.getGlyphID(name), font['head'].unitsPerEm,
          min(p[0] for p in points), min(p[1] for p in points),
          max(p[0] for p in points), max(p[1] for p in points), offcurve)
"#;
    let output = std::process::Command::new(python)
        .arg("-c")
        .arg(script)
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fontTools failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    println!("independent fontTools observations:\n{stdout}");
    let face = FontFace::load("fonttools-comparison", path).unwrap();
    let rows: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("glyph "))
        .collect();
    assert_eq!(rows.len(), 3);
    for row in rows {
        let fields: Vec<i32> = row
            .split_whitespace()
            .skip(1)
            .map(|field| field.parse().unwrap())
            .collect();
        assert_eq!(fields.len(), 8);
        let glyph_id = u32::try_from(fields[1]).unwrap();
        assert_ne!(glyph_id, 0);
        assert!(
            fields[7] > 0,
            "comparison glyph needs off-curve control points: {row}"
        );
        let evidence = face.glyph_bounds_evidence(glyph_id).unwrap();
        assert_eq!(i32::from(evidence.units_per_em), fields[2]);
        let OutlineObservation::Bounds(bounds) = evidence.observation else {
            panic!("no bounds for {row}")
        };
        assert!(
            bounds.x_min <= fields[3] as f32
                && bounds.y_min <= fields[4] as f32
                && bounds.x_max >= fields[5] as f32
                && bounds.y_max >= fields[6] as f32,
            "{row}, observed {bounds:?}"
        );
        println!("observer glyph {glyph_id}: {bounds:?}");
    }
}

#[test]
fn modified_tables_are_rejected_and_loaded_bytes_stay_immutable() {
    let path = std::env::var_os("BABELDOC_TEST_FONT").unwrap();
    let original = std::fs::read(path).unwrap();
    let table_count = u16::from_be_bytes([original[4], original[5]]) as usize;
    let name_record = (0..table_count)
        .map(|index| 12 + index * 16)
        .find(|&index| &original[index..index + 4] == b"name")
        .unwrap();
    let evidence_dir = std::env::var_os("GLYPH_BOUNDS_EVIDENCE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("glyph-bounds-evidence"));
    std::fs::create_dir_all(&evidence_dir).unwrap();
    let run_id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let save = |name: &str, bytes: &[u8]| {
        use std::io::Write;
        let path = evidence_dir.join(format!("{run_id}-{name}.ttf"));
        let mut file = std::fs::File::create_new(&path).unwrap();
        file.write_all(bytes).unwrap();
        path
    };
    let original_path = save("original", &original);
    let mutable_path = save("mutable", &original);
    let face = FontFace::load("immutable-test", &mutable_path).unwrap();
    let glyph = face.has_glyph('g' as u32);
    let baseline = face.glyph_bounds_evidence(glyph).unwrap();
    let mut variable = original.clone();
    variable[name_record..name_record + 4].copy_from_slice(b"fvar");
    let variable_path = save("variable", &variable);
    std::fs::write(&mutable_path, &variable).unwrap();
    assert_eq!(face.glyph_bounds_evidence(glyph), Ok(baseline));
    let altered = FontFace::load("variable-test", &variable_path).unwrap();
    assert_eq!(
        altered.glyph_bounds_evidence(glyph),
        Err(Failure::UnsupportedFont)
    );
    let record = |tag: &[u8; 4]| {
        (0..table_count)
            .map(|index| 12 + index * 16)
            .find(|&index| &original[index..index + 4] == tag)
            .unwrap()
    };
    let offset = |record: usize| {
        u32::from_be_bytes(original[record + 8..record + 12].try_into().unwrap()) as usize
    };
    let head = offset(record(b"head"));
    let loca = offset(record(b"loca"));
    let glyf = offset(record(b"glyf"));
    let glyph_offset =
        if u16::from_be_bytes(original[head + 50..head + 52].try_into().unwrap()) == 0 {
            2 * usize::from(u16::from_be_bytes(
                original[loca + glyph as usize * 2..loca + glyph as usize * 2 + 2]
                    .try_into()
                    .unwrap(),
            ))
        } else {
            u32::from_be_bytes(
                original[loca + glyph as usize * 4..loca + glyph as usize * 4 + 4]
                    .try_into()
                    .unwrap(),
            ) as usize
        };
    let mut malformed = original.clone();
    malformed[glyf + glyph_offset..glyf + glyph_offset + 2]
        .copy_from_slice(&i16::MAX.to_be_bytes());
    let malformed_path = save("malformed", &malformed);
    let altered = FontFace::load("malformed-test", &malformed_path).unwrap();
    assert_eq!(altered.glyph_bounds_evidence(glyph), Err(Failure::Parse));
    let mut oversized = original;
    oversized[glyf + glyph_offset..glyf + glyph_offset + 2].copy_from_slice(&1i16.to_be_bytes());
    oversized[glyf + glyph_offset + 10..glyf + glyph_offset + 12]
        .copy_from_slice(&32768u16.to_be_bytes());
    let oversized_path = save("limit", &oversized);
    let altered = FontFace::load("limit-test", &oversized_path).unwrap();
    assert_eq!(
        altered.glyph_bounds_evidence(glyph),
        Err(Failure::LimitExceeded)
    );
    assert!(original_path.exists());
}
