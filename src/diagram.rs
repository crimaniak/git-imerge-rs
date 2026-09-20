//! Rendering: ASCII diagrams (with axis labels), the legend text, and the
//! static HTML diagram.

use crate::block::{self, Grid};
use crate::frontier::{self, Frontier};

pub struct AnsiColor {
    pub enabled: bool,
}

impl AnsiColor {
    const D_GRAY: &'static str = "\x1b[1;30m";
    const B_RED: &'static str = "\x1b[1;31m";
    const B_GREEN: &'static str = "\x1b[1;32m";
    const B_YELLOW: &'static str = "\x1b[1;33m";
    const END: &'static str = "\x1b[0m";

    fn color(&self, code: &str, s: &str) -> String {
        if self.enabled {
            format!("{code}{s}{}", Self::END)
        } else {
            s.to_string()
        }
    }
}

/// Format a single diagram cell (as produced by `Block::create_diagram` or
/// `Frontier::create_diagram`) into a one-character-wide (possibly
/// ANSI-colored) string.
fn default_formatter(color: &AnsiColor, node: u8) -> String {
    let legend = ['?', '*', '.', '#', '@', '-', '|', '+'];
    let merge = node & block::MERGE_MASK;
    let within = merge == block::MERGE_MANUAL || (node & frontier::FRONTIER_WITHIN != 0);

    let pick = |s: &str, within: bool| -> String {
        if within {
            color.color(AnsiColor::B_GREEN, s)
        } else {
            color.color(AnsiColor::B_RED, s)
        }
    };

    let skip = [block::MERGE_MANUAL, block::MERGE_BLOCKED, block::MERGE_UNBLOCKED];
    if !skip.contains(&merge) {
        let vertex = frontier::FRONTIER_BOTTOM_EDGE | frontier::FRONTIER_RIGHT_EDGE;
        let edge_status = node & vertex;
        if edge_status == vertex {
            return pick(&legend[7].to_string(), within);
        } else if edge_status == frontier::FRONTIER_RIGHT_EDGE {
            return pick(&legend[6].to_string(), within);
        } else if edge_status == frontier::FRONTIER_BOTTOM_EDGE {
            return pick(&legend[5].to_string(), within);
        }
    }
    pick(&legend[merge as usize].to_string(), within)
}

/// Format a plain `Block`-style diagram (no frontier overlay bits): used
/// for `diagram --commits`.
fn plain_formatter(color: &AnsiColor, node: u8) -> String {
    match node {
        block::MERGE_UNKNOWN => color.color(AnsiColor::D_GRAY, "?"),
        block::MERGE_MANUAL => color.color(AnsiColor::B_GREEN, "*"),
        block::MERGE_AUTOMATIC => color.color(AnsiColor::B_GREEN, "."),
        block::MERGE_BLOCKED => color.color(AnsiColor::B_RED, "#"),
        block::MERGE_UNBLOCKED => color.color(AnsiColor::B_YELLOW, "@"),
        _ => "?".to_string(),
    }
}

fn format_diagram(diagram: &[Vec<u8>], color: &AnsiColor, frontier_style: bool) -> Vec<Vec<String>> {
    diagram
        .iter()
        .map(|row| {
            row.iter()
                .map(|&node| {
                    if frontier_style {
                        default_formatter(color, node)
                    } else {
                        plain_formatter(color, node)
                    }
                })
                .collect()
        })
        .collect()
}

/// Write a diagram of one-character-wide cells with row/column index axes,
/// matching the upstream tool's exact spacing (numbers every 5
/// rows/columns, `|` tick marks, tip names attached to row 0 / below the
/// grid).
pub fn write_diagram_with_axes(out: &mut dyn std::io::Write, diagram: &[Vec<String>], tip1: Option<&str>, tip2: Option<&str>) -> std::io::Result<()> {
    let len1 = diagram.len();
    let len2 = if len1 > 0 { diagram[0].len() } else { 0 };

    let last1 = len1.saturating_sub(1);
    let rem = last1 % 5;

    // Row of i1 numbers.
    write!(out, "   ")?;
    let mut i1 = 0;
    while i1 < len1 {
        write!(out, "{i1:>5}")?;
        i1 += 5;
    }
    if rem == 0 {
        writeln!(out)?;
    } else {
        if rem == 1 {
            write!(out, " ")?;
        }
        writeln!(out, "{}{}", " ".repeat(rem - 1), last1)?;
    }

    // Row of '|' tick marks.
    write!(out, "   ")?;
    let mut i1 = 0;
    while i1 < len1 {
        write!(out, "{:>5}", "|")?;
        i1 += 5;
    }
    if rem == 0 {
        writeln!(out)?;
    } else if rem == 1 {
        writeln!(out, " /")?;
    } else {
        writeln!(out, "{}|", " ".repeat(rem - 1))?;
    }

    // Body.
    for i2 in 0..len2 {
        if i2 % 5 == 0 || i2 == len2 - 1 {
            write!(out, "{i2:>4} - ")?;
        } else {
            write!(out, "       ")?;
        }
        for row in diagram.iter() {
            write!(out, "{}", row[i2])?;
        }
        if let Some(tip1) = tip1 {
            if i2 == 0 {
                writeln!(out, " - {tip1}")?;
                continue;
            }
        }
        writeln!(out)?;
    }

    if let Some(tip2) = tip2 {
        writeln!(out, "       |")?;
        writeln!(out, "     {tip2}")?;
    }

    Ok(())
}

pub fn write_commits_diagram(
    out: &mut dyn std::io::Write,
    grid: &Grid,
    tip1: &str,
    tip2: &str,
    color_enabled: bool,
) -> std::io::Result<()> {
    let color = AnsiColor { enabled: color_enabled };
    let diagram = grid.create_diagram();
    let formatted = format_diagram(&diagram, &color, false);
    write_diagram_with_axes(out, &formatted, Some(tip1), Some(tip2))
}

pub fn write_frontier_diagram(
    out: &mut dyn std::io::Write,
    grid: &Grid,
    frontier: &Frontier,
    tip1: &str,
    tip2: &str,
    color_enabled: bool,
) -> std::io::Result<()> {
    let color = AnsiColor { enabled: color_enabled };
    let diagram = frontier.create_diagram(grid);
    let formatted = format_diagram(&diagram, &color, true);
    write_diagram_with_axes(out, &formatted, Some(tip1), Some(tip2))
}

pub const LEGEND: &str = "\
  * = merge done manually
  . = merge done automatically
  # = conflict that is currently blocking progress
  @ = merge was blocked but has been resolved
  ? = no merge recorded
";

pub const FRONTIER_LEGEND_LINE: &str = "  |,-,+ = rectangles forming current merge frontier\n";

/// Write the static HTML diagram (a plain `<table>`, referencing an
/// external `imerge.css` the same way the upstream tool does).
pub fn write_html(
    out: &mut dyn std::io::Write,
    grid: &Grid,
    frontier: &Frontier,
    name: &str,
    cssfile: &str,
    abbrev_sha1: usize,
) -> std::io::Result<()> {
    writeln!(out, "<html>")?;
    writeln!(out, "<head>")?;
    writeln!(out, "<title>git-imerge: {name}</title>")?;
    writeln!(out, "<link rel=\"stylesheet\" href=\"{cssfile}\" type=\"text/css\" />")?;
    writeln!(out, "</head>")?;
    writeln!(out, "<body>")?;
    writeln!(out, "<table id=\"imerge\">")?;

    let diagram = frontier.create_diagram(grid);
    let top = frontier.block();

    writeln!(out, "  <tr>")?;
    writeln!(out, "    <th class=\"indexes\">&nbsp;</td>")?;
    for i1 in 0..top.len1 {
        writeln!(out, "    <th class=\"indexes\">{i1}-*</td>")?;
    }
    writeln!(out, "  </tr>")?;

    for i2 in 0..top.len2 {
        writeln!(out, "  <tr>")?;
        writeln!(out, "    <th class=\"indexes\">*-{i2}</td>")?;
        for i1 in 0..top.len1 {
            let node = diagram[i1][i2];
            let classes = map_to_classes(i1, i2, node, top.len1, top.len2);
            let (a1, a2) = (top.origin1 + i1, top.origin2 + i2);
            let rec = grid.get(a1, a2);
            let sha1 = rec.sha1.clone().unwrap_or_default();
            let td_id = if rec.sha1.is_some() {
                format!(" id=\"{sha1}\"")
            } else {
                String::new()
            };
            let td_class = if classes.is_empty() {
                String::new()
            } else {
                format!(" class=\"{}\"", classes.join(" "))
            };
            let shown: String = sha1.chars().take(abbrev_sha1).collect();
            writeln!(out, "    <td{td_id}{td_class}>{shown}</td>")?;
        }
        writeln!(out, "  </tr>")?;
    }
    writeln!(out, "</table>")?;
    writeln!(out, "</body>")?;
    writeln!(out, "</html>")?;
    Ok(())
}

fn map_to_classes(i1: usize, i2: usize, node: u8, len1: usize, len2: usize) -> Vec<String> {
    let merge = node & block::MERGE_MASK;
    let merge_class = match merge {
        block::MERGE_UNKNOWN => "merge_unknown",
        block::MERGE_MANUAL => "merge_manual",
        block::MERGE_AUTOMATIC => "merge_automatic",
        block::MERGE_BLOCKED => "merge_blocked",
        block::MERGE_UNBLOCKED => "merge_unblocked",
        _ => "merge_unknown",
    };
    let mut ret = vec![merge_class.to_string()];
    if node & frontier::FRONTIER_WITHIN != 0 {
        ret.push("frontier_within".to_string());
    }
    if node & frontier::FRONTIER_RIGHT_EDGE != 0 {
        ret.push("frontier_right_edge".to_string());
    }
    if node & frontier::FRONTIER_BOTTOM_EDGE != 0 {
        ret.push("frontier_bottom_edge".to_string());
    }
    if node & frontier::FRONTIER_WITHIN == 0 {
        ret.push("frontier_without".to_string());
    } else if merge == block::MERGE_UNKNOWN {
        ret.push("merge_skipped".to_string());
    }
    if i1 == 0 || i2 == 0 {
        ret.push("merge_initial".to_string());
    }
    if i1 == 0 {
        ret.push("col_left".to_string());
    }
    if i1 == len1 - 1 {
        ret.push("col_right".to_string());
    }
    if i2 == 0 {
        ret.push("row_top".to_string());
    }
    if i2 == len2 - 1 {
        ret.push("row_bottom".to_string());
    }
    ret
}
