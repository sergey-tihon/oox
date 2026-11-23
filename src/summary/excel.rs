//! Lightweight Excel package summary.
use std::io::{self, Read};

use quick_xml::{Reader, events::Event};

use super::{
    DetailsView, MAX_SHARED_STRINGS, MAX_SUMMARY_ITEMS, SummaryBuilder, SummaryText, element_is,
    read_part, relationship_target_for_id, validate_xml_bytes,
};
use crate::cancel::Cancel;
use crate::package::{PackageIndex, SHARED_STRINGS_REL_TYPES, xml_attribute};
pub(super) fn build_excel_summary<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
    main: &str,
    cancel: &Cancel,
) -> io::Result<DetailsView> {
    let workbook = read_part(archive, index, main)?;
    validate_xml_bytes(&workbook, main)?;
    let sheets = parse_excel_workbook(&workbook);
    let shared_strings = match index.related_part(main, &SHARED_STRINGS_REL_TYPES) {
        Some(part) => {
            let bytes = read_part(archive, index, &part.path)?;
            validate_xml_bytes(&bytes, &part.path)?;
            parse_shared_strings(&bytes)
        }
        None => Vec::new(),
    };

    let mut summary = SummaryBuilder::new();
    summary.push_line("Excel summary", None);
    summary.push_line("", None);
    summary.push_line(format!("Workbook: {main}"), Some(main));
    summary.push_line(format!("Sheets: {}", sheets.len()), None);
    for (name, relationship_id) in sheets {
        cancel.check()?;
        summary.push_line("", None);
        summary.push_line(format!("- {name}"), None);
        let Some(path) = relationship_target_for_id(index, main, &relationship_id) else {
            summary.push_line("  Worksheet part: unavailable", None);
            continue;
        };
        let worksheet = read_part(archive, index, &path)?;
        validate_xml_bytes(&worksheet, &path)?;
        let sheet_summary = parse_excel_worksheet(&worksheet, &shared_strings);
        summary.push_line(format!("  Worksheet: {path}"), Some(&path));
        summary.push_line(
            format!(
                "  Range: {}",
                sheet_summary.range.as_deref().unwrap_or("unknown")
            ),
            None,
        );
        summary.push_line(format!("  Cells: {}", sheet_summary.cells.len()), None);
        summary.push_line(format!("  Formulas: {}", sheet_summary.formula_count), None);
        for cell in sheet_summary.cells.iter().take(20) {
            if let Some(formula) = cell.formula.as_deref() {
                summary.push_line(format!("  {} = {formula}", cell.reference), None);
            } else if !cell.value.is_empty() {
                summary.push_line(format!("  {} = {}", cell.reference, cell.value), None);
            }
        }
        if sheet_summary.cells.len() > 20 {
            summary.push_line(
                format!("  ... {} more cells", sheet_summary.cells.len() - 20),
                None,
            );
        }
    }
    Ok(summary.finish())
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
                ) && sheets.len() < MAX_SUMMARY_ITEMS
                {
                    sheets.push((name, relationship_id));
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
    let mut current = SummaryText::new();
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
            Ok(Event::Text(event)) if in_text => current.push_text(&event),
            Ok(Event::GeneralRef(event)) if in_text => current.push_reference(&event),
            Ok(Event::End(event)) => {
                if element_is(event.name().as_ref(), b"t") {
                    in_text = false;
                } else if element_is(event.name().as_ref(), b"si") {
                    if values.len() < MAX_SHARED_STRINGS {
                        values.push(current.as_str().to_string());
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
    let mut value = SummaryText::new();
    let mut formula: Option<SummaryText> = None;
    let mut active_field = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = event.name();
                if element_is(name.as_ref(), b"dimension") {
                    summary.range = xml_attribute(&event, b"ref");
                } else if element_is(name.as_ref(), b"c") {
                    value.clear();
                    formula = None;
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
                if current_cell.is_some() {
                    match active_field {
                        Some(ExcelField::Value) => value.push_text(&event),
                        Some(ExcelField::Formula) => formula
                            .get_or_insert_with(SummaryText::new)
                            .push_text(&event),
                        None => {}
                    }
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if current_cell.is_some() {
                    match active_field {
                        Some(ExcelField::Value) => value.push_reference(&event),
                        Some(ExcelField::Formula) => formula
                            .get_or_insert_with(SummaryText::new)
                            .push_reference(&event),
                        None => {}
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
                } else if element_is(name.as_ref(), b"c")
                    && let Some(mut cell) = current_cell.take()
                {
                    cell.value = std::mem::replace(&mut value, SummaryText::new()).into_string();
                    cell.formula = formula.take().map(SummaryText::into_string);
                    if cell.cell_type.as_deref() == Some("s")
                        && let Ok(index) = cell.value.parse::<usize>()
                    {
                        cell.value = shared_strings.get(index).cloned().unwrap_or_default();
                    }
                    if cell.formula.is_some() {
                        summary.formula_count += 1;
                    }
                    if summary.cells.len() < MAX_SUMMARY_ITEMS {
                        summary.cells.push(cell);
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
