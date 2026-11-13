use babeldoc_writer_core::{PageObjectMap, PdfDocument, page_object_with_map};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    pub page_number: i64,
    pub form_xref: Option<i64>,
}

pub(crate) struct Evidence {
    pdf: PdfDocument,
    pages: PageObjectMap,
}

impl Evidence {
    pub(crate) fn load(path: &Path, digest: &[u8; 32], pages: &PageObjectMap) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        Self::from_bytes(&bytes, digest, pages)
    }

    pub(crate) fn from_bytes(bytes: &[u8], digest: &[u8; 32], pages: &PageObjectMap) -> Option<Self> {
        if pages.is_empty() {
            return None;
        }
        let actual: [u8; 32] = Sha256::digest(&bytes).into();
        if &actual != digest {
            return None;
        }
        let pdf = PdfDocument::load_mem(bytes).ok()?;
        Some(Self { pdf, pages: pages.clone() })
    }

    fn resource(&self, scope: Scope, kind: &[u8], name: &[u8]) -> Option<Option<(u32, u16)>> {
        let page = page_object_with_map(&self.pdf, &self.pages, scope.page_number).ok()?;
        let owner = if let Some(xref) = scope.form_xref {
            let number = u32::try_from(xref).ok()?;
            let mut matches = self.pdf.objects.keys().filter(|(value, _)| *value == number);
            let id = *matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            let form = self.pdf.get_object(id).ok()?.as_stream().ok()?;
            if form.dict.get(b"Subtype").ok()?.as_name().ok()? != b"Form" {
                return None;
            }
            id
        } else {
            page
        };
        let object = self.pdf.get_object(owner).ok()?;
        let owner = babeldoc_parser_core::dict(&self.pdf, object).ok()?;
        let resources = babeldoc_parser_core::dict(&self.pdf, owner.get(b"Resources").ok()?).ok()?;
        let category = match resources.get(kind) {
            Ok(value) if !value.is_null() => babeldoc_parser_core::dict(&self.pdf, value).ok()?,
            Ok(_) => return None,
            Err(_) => return Some(None),
        };
        let value = match category.get(name) {
            Ok(value) if !value.is_null() => value,
            Ok(_) => return None,
            Err(_) => return Some(None),
        };
        let id = value.as_reference().ok()?;
        self.pdf.get_object(id).ok()?;
        Some(Some(id))
    }

    fn same_resource(&self, source: Scope, target: Scope, kind: &[u8], name: &[u8]) -> Option<(u32, u16)> {
        let source = self.resource(source, kind, name)??;
        let target = self.resource(target, kind, name)??;
        (source == target).then_some(source)
    }

    fn same_default(&self, source: Scope, target: Scope, name: &[u8]) -> bool {
        match (self.resource(source, b"ColorSpace", name), self.resource(target, b"ColorSpace", name)) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }

    fn color_space(&self, source: Scope, target: Scope, name: &[u8]) -> Option<usize> {
        let id = self.same_resource(source, target, b"ColorSpace", name)?;
        let color = self.pdf.get_object(id).ok()?.as_array().ok()?;
        if color.len() != 2 || color[0].as_name().ok()? != b"ICCBased" {
            return None;
        }
        let profile = self.pdf.get_object(color[1].as_reference().ok()?).ok()?.as_stream().ok()?;
        let channels = profile.dict.get(b"N").ok()?.as_i64().ok()?;
        let (default, expected_alternate) = match channels {
            1 => (b"DefaultGray".as_slice(), b"DeviceGray".as_slice()),
            3 => (b"DefaultRGB".as_slice(), b"DeviceRGB".as_slice()),
            4 => (b"DefaultCMYK".as_slice(), b"DeviceCMYK".as_slice()),
            _ => return None,
        };
        if let Ok(alternate) = profile.dict.get(b"Alternate") {
            if alternate.as_name().ok()? != expected_alternate {
                return None;
            }
        }
        if profile.get_plain_content_with_limit(16 * 1024 * 1024).is_err() {
            return None;
        }
        if !self.same_default(source, target, default) {
            return None;
        }
        usize::try_from(channels).ok()
    }

    fn graphics_state(&self, source: Scope, target: Scope, name: &[u8]) -> bool {
        let Some(id) = self.same_resource(source, target, b"ExtGState", name) else {
            return false;
        };
        let Ok(object) = self.pdf.get_object(id) else { return false };
        let Ok(dictionary) = object.as_dict() else { return false };
        if source != target && dictionary.get(b"BM").ok().and_then(|value| value.as_name().ok())
            .is_some_and(|name| name != b"Normal") {
            return false;
        }
        dictionary.iter().all(|(key, value)| match key.as_slice() {
            b"Type" => value.as_name().is_ok_and(|name| name == b"ExtGState"),
            b"SMask" => value.as_name().is_ok_and(|name| name == b"None"),
            b"BM" => value.as_name().is_ok_and(|name| matches!(name,
                b"Normal" | b"Multiply" | b"Screen" | b"Overlay" | b"Darken" | b"Lighten"
                    | b"ColorDodge" | b"ColorBurn" | b"HardLight" | b"SoftLight"
                    | b"Difference" | b"Exclusion" | b"Hue" | b"Saturation" | b"Color" | b"Luminosity")),
            b"CA" | b"ca" => value.as_float().is_ok_and(|number| number.is_finite() && number == 1.0),
            b"OPM" => value.as_i64().is_ok_and(|number| matches!(number, 0 | 1)),
            b"OP" | b"op" | b"AIS" | b"SA" | b"TK" => value.as_bool().is_ok(),
            _ => false,
        })
    }

    pub(crate) fn transferable(&self, instructions: &str, source: Scope, target: Scope) -> bool {
        let Some(tokens) = tokenize(instructions) else { return false };
        let mut operands: Vec<Token> = Vec::new();
        let mut fill_channels = None;
        let mut stroke_channels = None;
        for token in tokens {
            let Token::Operator(operator) = token else {
                operands.push(token);
                continue;
            };
            let valid = match operator.as_str() {
                "cs" | "CS" => {
                    let channels = match operands.as_slice() {
                        [Token::Name(name)] => self.color_space(source, target, name),
                        _ => None,
                    };
                    if operator == "cs" { fill_channels = channels; } else { stroke_channels = channels; }
                    channels.is_some()
                }
                "gs" => match operands.as_slice() {
                    [Token::Name(name)] => self.graphics_state(source, target, name),
                    _ => false,
                },
                "sc" | "scn" => fill_channels.is_some_and(|count| numeric_operands(&operands, count)),
                "SC" | "SCN" => stroke_channels.is_some_and(|count| numeric_operands(&operands, count)),
                "g" | "G" | "rg" | "RG" | "k" | "K" => {
                    let (count, default) = match operator.as_str() {
                        "g" | "G" => (1, b"DefaultGray".as_slice()),
                        "rg" | "RG" => (3, b"DefaultRGB".as_slice()),
                        _ => (4, b"DefaultCMYK".as_slice()),
                    };
                    if operator.bytes().all(|byte| byte.is_ascii_lowercase()) {
                        fill_channels = Some(count);
                    } else {
                        stroke_channels = Some(count);
                    }
                    numeric_operands(&operands, count) && self.same_default(source, target, default)
                }
                "Tr" => matches!(operands.as_slice(), [Token::Number(value)] if matches!(*value, 0.0 | 1.0 | 2.0 | 3.0)),
                "J" | "j" => matches!(operands.as_slice(), [Token::Number(value)] if matches!(*value, 0.0 | 1.0 | 2.0)),
                "w" => matches!(operands.as_slice(), [Token::Number(value)] if *value >= 0.0),
                "M" => matches!(operands.as_slice(), [Token::Number(value)] if *value >= 1.0),
                "d" => matches!(operands.as_slice(), [Token::Array(values), Token::Number(phase)] if *phase >= 0.0 && values.iter().all(|value| *value >= 0.0)),
                "Tz" => matches!(operands.as_slice(), [Token::Number(value)] if *value > 0.0),
                "Tc" | "Tw" | "TL" | "Ts" => numeric_operands(&operands, 1),
                _ => false,
            };
            if !valid {
                return false;
            }
            operands.clear();
        }
        operands.is_empty()
    }
}

#[derive(Debug)]
enum Token {
    Number(f64),
    Name(Vec<u8>),
    Array(Vec<f64>),
    Operator(String),
}

fn numeric_operands(operands: &[Token], count: usize) -> bool {
    operands.len() == count && operands.iter().all(|value| matches!(value, Token::Number(_)))
}

fn number(token: &str) -> Option<f64> {
    let bytes = token.as_bytes();
    let mut digits = 0;
    let mut dots = 0;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'0'..=b'9' => digits += 1,
            b'.' => dots += 1,
            b'+' | b'-' if index == 0 => (),
            _ => return None,
        }
    }
    if digits == 0 || dots > 1 { return None; }
    token.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn name(token: &str) -> Option<Vec<u8>> {
    let bytes = token.strip_prefix('/')?.as_bytes();
    if bytes.is_empty() { return None; }
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'#' {
            let digits = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(digits, 16).ok()?);
            index += 3;
        } else {
            if !byte.is_ascii_graphic() || b"()<>[]{}/%".contains(&byte) { return None; }
            decoded.push(byte);
            index += 1;
        }
    }
    Some(decoded)
}

fn tokenize(instructions: &str) -> Option<Vec<Token>> {
    let normalized = instructions.replace('[', " [ ").replace(']', " ] ");
    let mut result = Vec::new();
    let mut array: Option<Vec<f64>> = None;
    for token in normalized.split_whitespace() {
        match token {
            "[" if array.is_none() => array = Some(Vec::new()),
            "]" => result.push(Token::Array(array.take()?)),
            _ if array.is_some() => array.as_mut()?.push(number(token)?),
            _ if token.starts_with('/') => result.push(Token::Name(name(token)?)),
            _ if number(token).is_some() => result.push(Token::Number(number(token)?)),
            _ if token.bytes().all(|byte| byte.is_ascii_alphabetic()) => result.push(Token::Operator(token.to_owned())),
            _ => return None,
        }
    }
    array.is_none().then_some(result)
}
