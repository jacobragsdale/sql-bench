//! Result sets as text: an aligned table, CSV, JSON.
//!
//! One place, because the same rows leave through three doors — the headless
//! subcommands, and later the grid's export key — and a value that reads one
//! way in a terminal and another in a file is a bug waiting for someone
//! else's spreadsheet. Everything here is a pure function of the rows, so a
//! test needs no database.

use std::borrow::Cow;
use std::fmt::Write as _;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::db::model::{Cell, Column};

/// How much of a cell a terminal wants to see before the rest is somebody
/// else's problem. `--full` turns it off.
pub const CELL_LIMIT: usize = 60;

/// How many terminal columns a string is drawn in — the one width the grid
/// and this module both measure with, so a table and the pane it came from
/// line up on the same glyphs. A CJK name is two columns per character.
#[must_use]
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// The start of `text` that is at most `columns` wide, with `…` for the rest
/// when there is one. The `…` is a column of its own, so a cut never spills
/// over the width it was given — and a wide glyph that would straddle the
/// end is dropped rather than halved.
#[must_use]
pub fn cut_to(text: &str, columns: usize) -> Cow<'_, str> {
    if columns == 0 || width(text) <= columns {
        return Cow::Borrowed(text);
    }
    let mut kept = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cost = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + cost > columns - 1 {
            break;
        }
        kept.push(character);
        used += cost;
    }
    kept.push('…');
    Cow::Owned(kept)
}

/// `text` padded to `columns` terminal columns, on the side that leaves the
/// value where it reads best. Rust's own `{:width$}` counts characters, which
/// is the bug this whole helper exists to keep out of two renderers.
#[must_use]
pub fn pad(text: &str, columns: usize, right: bool) -> String {
    let padding = " ".repeat(columns.saturating_sub(width(text)));
    if right {
        padding + text
    } else {
        text.to_owned() + &padding
    }
}

/// At most `limit` columns, the last of which says there were more.
fn cut(text: &str, limit: Option<usize>) -> Cow<'_, str> {
    limit.map_or(Cow::Borrowed(text), |limit| cut_to(text, limit))
}

/// What a cell reads as in a table: the one difference from
/// [`Cell::display`] is that a NULL is worth saying out loud when a column
/// of empty strings sits next to it.
fn shown(cell: Option<&Cell>, limit: Option<usize>) -> Cow<'_, str> {
    match cell {
        Some(Cell::Null) => Cow::Borrowed("NULL"),
        Some(cell) => match cell.display() {
            Cow::Borrowed(text) => cut(text, limit),
            Cow::Owned(text) => Cow::Owned(cut(&text, limit).into_owned()),
        },
        None => Cow::Borrowed(""),
    }
}

/// A column holds numbers when something in it is one and nothing in it is
/// anything else; those are the columns that read better right-aligned.
fn numeric(rows: &[Vec<Cell>], index: usize) -> bool {
    let mut seen = false;
    for row in rows {
        match row.get(index) {
            Some(Cell::Int(_) | Cell::Float(_) | Cell::Decimal(_)) => seen = true,
            None | Some(Cell::Null) => {}
            Some(_) => return false,
        }
    }
    seen
}

/// Header, rule, rows: columns padded to their widest value, numbers to the
/// right. `limit` is how many characters of a cell to show, [`None`] for all
/// of it.
#[must_use]
pub fn table(columns: &[Column], rows: &[Vec<Cell>], limit: Option<usize>) -> String {
    if columns.is_empty() {
        return String::new();
    }
    let cells: Vec<Vec<Cow<'_, str>>> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|index| shown(row.get(index), limit))
                .collect()
        })
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            cells
                .iter()
                .map(|row| width(&row[index]))
                .chain(std::iter::once(width(&column.name)))
                .max()
                .unwrap_or_default()
        })
        .collect();
    let right: Vec<bool> = (0..columns.len())
        .map(|index| numeric(rows, index))
        .collect();

    let mut out = String::new();
    let header: Vec<Cow<'_, str>> = columns
        .iter()
        .map(|column| Cow::Borrowed(column.name.as_str()))
        .collect();
    line(&mut out, &header, &widths, &[]);
    let rule: Vec<Cow<'_, str>> = widths.iter().map(|n| Cow::Owned("-".repeat(*n))).collect();
    line(&mut out, &rule, &widths, &[]);
    for row in &cells {
        line(&mut out, row, &widths, &right);
    }
    out
}

/// One row of a table, padded and trimmed of the trailing run of spaces the
/// last column would otherwise leave on every line.
fn line(out: &mut String, cells: &[Cow<'_, str>], widths: &[usize], right: &[bool]) {
    let mut row = String::new();
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            row.push_str("  ");
        }
        row.push_str(&pad(
            cell,
            widths[index],
            right.get(index).copied().unwrap_or(false),
        ));
    }
    out.push_str(row.trim_end());
    out.push('\n');
}

/// RFC 4180: a field is quoted when it holds a quote, a comma or a line
/// break, and a quote inside one is doubled. Lines end with `\n` rather than
/// the RFC's CRLF, because this is a Unix tool writing to a Unix pipe and
/// every reader takes both.
#[must_use]
pub fn csv(columns: &[Column], rows: &[Vec<Cell>]) -> String {
    let mut out = String::new();
    if columns.is_empty() {
        return out;
    }
    csv_row(&mut out, columns.iter().map(|column| column.name.as_str()));
    for row in rows {
        csv_row(
            &mut out,
            (0..columns.len()).map(|index| match row.get(index) {
                Some(cell) => cell.display(),
                None => Cow::Borrowed(""),
            }),
        );
    }
    out
}

fn csv_row<'a>(out: &mut String, fields: impl Iterator<Item = impl Into<Cow<'a, str>>>) {
    delimited_row(out, ',', fields);
}

/// One line of tab-separated fields, quoted the way [`csv`] quotes with the
/// tab in the comma's place: a spreadsheet reads that back as one cell per
/// field, a value with a tab or a line break in it included.
pub fn tsv_row<'a>(out: &mut String, fields: impl Iterator<Item = impl Into<Cow<'a, str>>>) {
    delimited_row(out, '\t', fields);
}

fn delimited_row<'a>(
    out: &mut String,
    separator: char,
    fields: impl Iterator<Item = impl Into<Cow<'a, str>>>,
) {
    for (index, field) in fields.enumerate() {
        if index > 0 {
            out.push(separator);
        }
        let field: Cow<'a, str> = field.into();
        if field.contains(['"', separator, '\n', '\r']) {
            out.push('"');
            for c in field.chars() {
                if c == '"' {
                    out.push('"');
                }
                out.push(c);
            }
            out.push('"');
        } else {
            out.push_str(&field);
        }
    }
    out.push('\n');
}

/// An array of objects, one row to a line so that a thousand of them are
/// still greppable. NULL is `null`, a boolean is a boolean and a number is a
/// number; everything else — including a decimal with a scale, whose digits
/// and trailing zeros a JSON reader would round away — is a string, which is
/// what [`Cell`] promises. A whole decimal a double holds exactly (Oracle's
/// `count(*)`, a `NUMBER` id) is a number, because it is one.
#[must_use]
pub fn json(columns: &[Column], rows: &[Vec<Cell>]) -> String {
    if rows.is_empty() {
        return "[]\n".to_owned();
    }
    let keys = keys(columns);
    let mut out = String::from("[\n");
    for (index, row) in rows.iter().enumerate() {
        out.push_str("  {");
        for (at, key) in keys.iter().enumerate() {
            if at > 0 {
                out.push_str(", ");
            }
            string(&mut out, key);
            out.push_str(": ");
            value(&mut out, row.get(at));
        }
        out.push('}');
        if index + 1 < rows.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

/// The column names as JSON keys, made unique. A result set may repeat a
/// name — `select 1 as a, 2 as a` is legal on both servers, and so is a join
/// of two tables with an `id` each — and every parser keeps only the last of
/// two equal keys, so a column would go missing. The table and CSV formats
/// are positional and need none of this.
fn keys(columns: &[Column]) -> Vec<String> {
    let mut used = std::collections::HashSet::with_capacity(columns.len());
    let mut keys = Vec::with_capacity(columns.len());
    for column in columns {
        let mut key = column.name.clone();
        let mut repeat = 1;
        // A suffix can itself collide (`a`, `a_2`, `a`), so it counts on.
        while !used.insert(key.clone()) {
            repeat += 1;
            key = format!("{}_{repeat}", column.name);
        }
        keys.push(key);
    }
    keys
}

fn value(out: &mut String, cell: Option<&Cell>) {
    match cell {
        None | Some(Cell::Null) => out.push_str("null"),
        Some(Cell::Int(number)) => {
            let _ = write!(out, "{number}");
        }
        // JSON has no infinity and no NaN; a value that is neither a number
        // nor a string is the one thing left.
        Some(Cell::Float(number)) if number.is_finite() => {
            let _ = write!(out, "{number}");
        }
        Some(Cell::Float(_)) => out.push_str("null"),
        Some(Cell::Bool(value)) => out.push_str(if *value { "true" } else { "false" }),
        // 2^53: past it a JavaScript reader rounds, which is the rounding a
        // string is there to prevent. Printed back the same, so `+5` and
        // `007`, which JSON has no spelling for, stay strings.
        Some(Cell::Decimal(text))
            if text
                .parse::<i64>()
                .is_ok_and(|n| n.unsigned_abs() <= 1 << 53 && n.to_string() == *text) =>
        {
            out.push_str(text);
        }
        Some(cell) => string(out, &cell.display()),
    }
}

/// RFC 8259 section 7: the two mandatory escapes, the short forms, and
/// `\u00xx` for the rest of the control characters. Everything above them is
/// UTF-8 already and goes through as it is.
fn string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .map(|name| Column {
                name: (*name).to_owned(),
                type_name: "int".to_owned(),
            })
            .collect()
    }

    fn text(value: &str) -> Cell {
        Cell::Text(value.to_owned())
    }

    #[test]
    fn a_table_pads_every_column_to_its_widest_value() {
        let table = table(
            &columns(&["id", "name"]),
            &[
                vec![Cell::Int(1), text("Zoë Bauer")],
                vec![Cell::Int(1000), text("Bo")],
            ],
            Some(CELL_LIMIT),
        );
        assert_eq!(
            table,
            "id    name\n\
             ----  ---------\n\
             \x20  1  Zoë Bauer\n\
             1000  Bo\n"
        );
    }

    #[test]
    fn numbers_go_right_and_text_goes_left() {
        let table = table(
            &columns(&["n", "d", "f", "t"]),
            &[
                vec![
                    Cell::Int(7),
                    Cell::Decimal("1.50".to_owned()),
                    Cell::Float(0.5),
                    text("x"),
                ],
                vec![
                    Cell::Int(1234),
                    Cell::Decimal("10.00".to_owned()),
                    Cell::Float(12.25),
                    text("yyyy"),
                ],
            ],
            None,
        );
        assert_eq!(
            table,
            "n     d      f      t\n\
             ----  -----  -----  ----\n\
             \x20  7   1.50    0.5  x\n\
             1234  10.00  12.25  yyyy\n"
        );
    }

    #[test]
    fn a_null_says_so_and_does_not_make_a_number_column_text() {
        let table = table(
            &columns(&["n", "t"]),
            &[vec![Cell::Null, Cell::Null], vec![Cell::Int(1), text("")]],
            None,
        );
        assert_eq!(
            table,
            "n     t\n\
             ----  ----\n\
             NULL  NULL\n\
             \x20  1\n"
        );
    }

    #[test]
    fn a_long_cell_is_cut_at_the_limit_including_the_ellipsis() {
        let long = "a".repeat(100);
        let cut = table(&columns(&["t"]), &[vec![text(&long)]], Some(CELL_LIMIT));
        let cut = cut.lines().nth(2).unwrap();
        assert_eq!(cut.chars().count(), CELL_LIMIT);
        assert!(cut.ends_with('…'), "{cut}");
        assert_eq!(&cut[..10], "aaaaaaaaaa");

        let whole = table(&columns(&["t"]), &[vec![text(&long)]], None);
        assert_eq!(whole.lines().nth(2).unwrap(), long);
    }

    /// T5.4: the limit is terminal columns, so a cut lands between two wide
    /// glyphs and never inside one — three of them and the `…` would be
    /// seven columns in a table four wide.
    #[test]
    fn a_cut_lands_on_a_character_and_not_in_the_middle_of_one() {
        let long = "李".repeat(100);
        let table = table(&columns(&["t"]), &[vec![text(&long)]], Some(4));
        assert_eq!(table.lines().nth(2).unwrap(), "李…");
        assert_eq!(width(table.lines().nth(2).unwrap()), 3);
    }

    /// T5.4: the column after a CJK value used to lean, because a name two
    /// terminal columns wide was padded as though it were one.
    #[test]
    fn a_wide_glyph_takes_two_columns_and_the_column_after_it_still_lines_up() {
        let rows = vec![
            vec![text("李雷"), text("CN")],
            vec![text("Zoe"), text("DE")],
            vec![text("山田太郎"), text("JP")],
        ];
        let table = table(&columns(&["name", "country"]), &rows, None);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(
            lines,
            [
                "name      country",
                "--------  -------",
                "李雷      CN",
                "Zoe       DE",
                "山田太郎  JP",
            ]
        );
        for line in &lines {
            assert_eq!(
                width(&line[..line.find("  ").unwrap()]).max(8),
                8,
                "the name column is eight columns wide on every line: {line:?}"
            );
        }
    }

    #[test]
    fn nothing_at_all_is_not_a_panic() {
        assert_eq!(table(&[], &[], Some(CELL_LIMIT)), "");
        assert_eq!(csv(&[], &[]), "");
        assert_eq!(json(&[], &[]), "[]\n");
        assert_eq!(table(&columns(&["id"]), &[], None), "id\n--\n");
        assert_eq!(csv(&columns(&["id"]), &[]), "id\n");
        assert_eq!(json(&columns(&["id"]), &[]), "[]\n");
        // A row shorter than the header is a driver bug, not a crash.
        assert_eq!(
            table(&columns(&["a", "b"]), &[vec![]], None),
            "a  b\n-  -\n\n"
        );
    }

    #[test]
    fn csv_quotes_only_what_rfc_4180_says_to() {
        let csv = csv(
            &columns(&["plain", "comma", "quote", "break", "null"]),
            &[vec![
                text("Zoë"),
                text("a,b"),
                text("say \"hi\""),
                text("one\ntwo"),
                Cell::Null,
            ]],
        );
        assert_eq!(
            csv,
            "plain,comma,quote,break,null\n\
             Zoë,\"a,b\",\"say \"\"hi\"\"\",\"one\ntwo\",\n"
        );
    }

    #[test]
    fn a_column_name_is_quoted_like_any_other_field() {
        let odd = vec![Column {
            name: "a,b".to_owned(),
            type_name: "int".to_owned(),
        }];
        assert_eq!(csv(&odd, &[]), "\"a,b\"\n");
    }

    #[test]
    fn json_writes_numbers_as_numbers_and_everything_else_as_strings() {
        let json = json(
            &columns(&["n", "f", "d", "w", "huge", "b", "t", "null", "bytes"]),
            &[vec![
                Cell::Int(-7),
                Cell::Float(1.5),
                Cell::Decimal("10.2500".to_owned()),
                Cell::Decimal("-42".to_owned()),
                Cell::Decimal("12345678901234567890".to_owned()),
                Cell::Bool(true),
                text("hi"),
                Cell::Null,
                Cell::Bytes(vec![0x00, 0xff]),
            ]],
        );
        assert_eq!(
            json,
            "[\n  {\"n\": -7, \"f\": 1.5, \"d\": \"10.2500\", \"w\": -42, \
             \"huge\": \"12345678901234567890\", \"b\": true, \
             \"t\": \"hi\", \"null\": null, \"bytes\": \"0x00ff\"}\n]\n"
        );
    }

    #[test]
    fn json_never_writes_the_same_key_twice() {
        // `select 1 as a, 2 as a` is legal, and an object with two `a` keys
        // loses one of them in every parser there is.
        let json = json(
            &columns(&["a", "a", "a_2", "b"]),
            &[vec![Cell::Int(1), Cell::Int(2), Cell::Int(3), Cell::Int(4)]],
        );
        assert_eq!(
            json.lines().nth(1).unwrap(),
            "  {\"a\": 1, \"a_2\": 2, \"a_2_2\": 3, \"b\": 4}"
        );
    }

    #[test]
    fn json_escapes_what_rfc_8259_requires_and_passes_the_rest_through() {
        let json = json(
            &columns(&["t"]),
            &[vec![text("q\"b\\s\nl\rr\tt\u{8}b\u{c}f\u{1}x 李 é")]],
        );
        assert_eq!(
            json.lines().nth(1).unwrap(),
            "  {\"t\": \"q\\\"b\\\\s\\nl\\rr\\tt\\bb\\ff\\u0001x 李 é\"}"
        );
    }

    #[test]
    fn json_has_no_word_for_infinity() {
        let json = json(
            &columns(&["f"]),
            &[
                vec![Cell::Float(f64::NAN)],
                vec![Cell::Float(f64::INFINITY)],
            ],
        );
        assert_eq!(json, "[\n  {\"f\": null},\n  {\"f\": null}\n]\n");
    }

    #[test]
    fn several_rows_are_one_array_of_one_object_a_line() {
        let json = json(&columns(&["id"]), &[vec![Cell::Int(1)], vec![Cell::Int(2)]]);
        assert_eq!(json, "[\n  {\"id\": 1},\n  {\"id\": 2}\n]\n");
    }
}
