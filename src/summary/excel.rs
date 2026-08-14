//! Lightweight Excel package summary.
use std::io::{self, Read};

use quick_xml::{events::Event, Reader};

use super::{
    append_decoded_reference, append_decoded_text, element_is, push_summary_line, read_part,
    relationship_target_for_id, validate_xml_bytes, DetailsView, MAX_SHARED_STRINGS,
    MAX_SUMMARY_ITEMS, MAX_SUMMARY_TEXT_CHARS,
};
use crate::package::{xml_attribute, PackageIndex};

pub(super) fn build_excel_summary<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
) -> io::Result<DetailsView> {
    let workbook = read_part(archive, index, "/xl/workbook.xml")?;
    validate_xml_bytes(&workbook, "/xl/workbook.xml")?;
    let sheets = parse_excel_workbook(&workbook);
    let shared_strings = index
        .parts
        .contains_key("/xl/sharedStrings.xml")
        .then(|| read_part(archive, index, "/xl/sharedStrings.xml"))
        .transpose()?
        .map(|bytes| {
            validate_xml_bytes(&bytes, "/xl/sharedStrings.xml")?;
            Ok::<Vec<String>, io::Error>(parse_shared_strings(&bytes))
        })
        .transpose()?;
    let shared_strings = shared_strings.unwrap_or_default();

    let mut summary = DetailsView {
        text: String::new(),
        links: Vec::new(),
    };
    push_summary_line(&mut summary, "Excel summary", None);
    push_summary_line(&mut summary, "", None);
    push_summary_line(
        &mut summary,
        "Workbook: /xl/workbook.xml",
        Some("/xl/workbook.xml"),
    );
    push_summary_line(&mut summary, format!("Sheets: {}", sheets.len()), None);
    for (name, relationship_id) in sheets {
        push_summary_line(&mut summary, "", None);
        push_summary_line(&mut summary, format!("- {name}"), None);
        let Some(path) = relationship_target_for_id(index, "/xl/workbook.xml", &relationship_id)
        else {
            push_summary_line(&mut summary, "  Worksheet part: unavailable", None);
            continue;
        };
        let worksheet = read_part(archive, index, &path)?;
        validate_xml_bytes(&worksheet, &path)?;
        let sheet_summary = parse_excel_worksheet(&worksheet, &shared_strings);
        push_summary_line(&mut summary, format!("  Worksheet: {path}"), Some(&path));
        push_summary_line(
            &mut summary,
            format!(
                "  Range: {}",
                sheet_summary.range.as_deref().unwrap_or("unknown")
            ),
            None,
        );
        push_summary_line(
            &mut summary,
            format!("  Cells: {}", sheet_summary.cells.len()),
            None,
        );
        push_summary_line(
            &mut summary,
            format!("  Formulas: {}", sheet_summary.formula_count),
            None,
        );
        for cell in sheet_summary.cells.iter().take(20) {
            if let Some(formula) = cell.formula.as_deref() {
                push_summary_line(
                    &mut summary,
                    format!("  {} = {formula}", cell.reference),
                    None,
                );
            } else if !cell.value.is_empty() {
                push_summary_line(
                    &mut summary,
                    format!("  {} = {}", cell.reference, cell.value),
                    None,
                );
            }
        }
        if sheet_summary.cells.len() > 20 {
            push_summary_line(
                &mut summary,
                format!("  ... {} more cells", sheet_summary.cells.len() - 20),
                None,
            );
        }
    }
    Ok(summary)
}

fn parse_excel_workbook(xml: &[u8]) -> Vec<(String, String)> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut sheets = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event))
                if element_is(event.name().as_ref(), b"sheet") =>
            {
                if let (Some(name), Some(relationship_id)) = (
                    xml_attribute(&event, b"name"),
                    xml_attribute(&event, b"r:id"),
                ) {
                    if sheets.len() < MAX_SUMMARY_ITEMS {
                        sheets.push((name, relationship_id));
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    sheets
}

fn parse_shared_strings(xml: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut in_text = false;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                if element_is(event.name().as_ref(), b"si") {
                    current.clear();
                    in_string = true;
                } else if in_string && element_is(event.name().as_ref(), b"t") {
                    in_text = true;
                }
            }
            Ok(Event::Text(event)) if in_text => {
                append_decoded_text(&mut current, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::GeneralRef(event)) if in_text => {
                append_decoded_reference(&mut current, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::End(event)) => {
                if element_is(event.name().as_ref(), b"t") {
                    in_text = false;
                } else if element_is(event.name().as_ref(), b"si") {
                    if values.len() < MAX_SHARED_STRINGS {
                        values.push(current.clone());
                    }
                    in_string = false;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    values
}

#[derive(Default)]
struct ExcelWorksheetSummary {
    range: Option<String>,
    cells: Vec<ExcelCell>,
    formula_count: usize,
}

#[derive(Default)]
struct ExcelCell {
    reference: String,
    cell_type: Option<String>,
    value: String,
    formula: Option<String>,
}

#[derive(Clone, Copy)]
enum ExcelField {
    Value,
    Formula,
}

fn parse_excel_worksheet(xml: &[u8], shared_strings: &[String]) -> ExcelWorksheetSummary {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut summary = ExcelWorksheetSummary::default();
    let mut current_cell = None;
    let mut active_field = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = event.name();
                if element_is(name.as_ref(), b"dimension") {
                    summary.range = xml_attribute(&event, b"ref");
                } else if element_is(name.as_ref(), b"c") {
                    current_cell = Some(ExcelCell {
                        reference: xml_attribute(&event, b"r").unwrap_or_default(),
                        cell_type: xml_attribute(&event, b"t"),
                        ..ExcelCell::default()
                    });
                } else if current_cell.is_some() && element_is(name.as_ref(), b"v") {
                    active_field = Some(ExcelField::Value);
                } else if current_cell.is_some() && element_is(name.as_ref(), b"f") {
                    active_field = Some(ExcelField::Formula);
                } else if current_cell.is_some() && element_is(name.as_ref(), b"t") {
                    active_field = Some(ExcelField::Value);
                }
            }
            Ok(Event::Empty(event)) => {
                if element_is(event.name().as_ref(), b"dimension") {
                    summary.range = xml_attribute(&event, b"ref");
                } else if element_is(event.name().as_ref(), b"c")
                    && summary.cells.len() < MAX_SUMMARY_ITEMS
                {
                    summary.cells.push(ExcelCell {
                        reference: xml_attribute(&event, b"r").unwrap_or_default(),
                        cell_type: xml_attribute(&event, b"t"),
                        ..ExcelCell::default()
                    });
                }
            }
            Ok(Event::Text(event)) => {
                if let (Some(cell), Some(field)) = (current_cell.as_mut(), active_field) {
                    match field {
                        ExcelField::Value => {
                            append_decoded_text(&mut cell.value, &event, MAX_SUMMARY_TEXT_CHARS)
                        }
                        ExcelField::Formula => {
                            append_decoded_text(
                                cell.formula.get_or_insert_with(String::new),
                                &event,
                                MAX_SUMMARY_TEXT_CHARS,
                            );
                        }
                    }
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if let (Some(cell), Some(field)) = (current_cell.as_mut(), active_field) {
                    match field {
                        ExcelField::Value => append_decoded_reference(
                            &mut cell.value,
                            &event,
                            MAX_SUMMARY_TEXT_CHARS,
                        ),
                        ExcelField::Formula => append_decoded_reference(
                            cell.formula.get_or_insert_with(String::new),
                            &event,
                            MAX_SUMMARY_TEXT_CHARS,
                        ),
                    }
                }
            }
            Ok(Event::End(event)) => {
                let name = event.name();
                if element_is(name.as_ref(), b"v")
                    || element_is(name.as_ref(), b"f")
                    || element_is(name.as_ref(), b"t")
                {
                    active_field = None;
                } else if element_is(name.as_ref(), b"c") {
                    if let Some(mut cell) = current_cell.take() {
                        if cell.cell_type.as_deref() == Some("s") {
                            if let Ok(index) = cell.value.parse::<usize>() {
                                cell.value = shared_strings.get(index).cloned().unwrap_or_default();
                            }
                        }
                        if cell.formula.is_some() {
                            summary.formula_count += 1;
                        }
                        if summary.cells.len() < MAX_SUMMARY_ITEMS {
                            summary.cells.push(cell);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_worksheet_parser_extracts_cells_and_formulas() {
        let excel = br#"
            <worksheet xmlns:r="relationships">
              <dimension ref="A1:B2"/>
              <sheetData>
                <row>
                  <c r="A1" t="inlineStr"><is><t>Hello</t></is></c>
                  <c r="B1"><f>SUM(A2:A3)</f><v>3</v></c>
                </row>
              </sheetData>
            </worksheet>
        "#;
        let worksheet = parse_excel_worksheet(excel, &[]);
        assert_eq!(worksheet.range.as_deref(), Some("A1:B2"));
        assert_eq!(worksheet.cells.len(), 2);
        assert_eq!(worksheet.formula_count, 1);
        assert_eq!(worksheet.cells[0].value, "Hello");
        assert_eq!(worksheet.cells[1].formula.as_deref(), Some("SUM(A2:A3)"));
    }

    #[test]
    fn shared_string_cells_are_resolved() {
        let shared = br#"
            <sst>
              <si><t>World</t></si>
            </sst>
        "#;
        let strings = parse_shared_strings(shared);
        assert_eq!(strings, vec!["World".to_string()]);

        let sheet = br#"
            <worksheet>
              <sheetData>
                <row><c r="A1" t="s"><v>0</v></c></row>
              </sheetData>
            </worksheet>
        "#;
        let worksheet = parse_excel_worksheet(sheet, &strings);
        assert_eq!(worksheet.cells[0].value, "World");
    }
}
