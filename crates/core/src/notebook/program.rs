//! A notebook's code cells as one program, which a language server reads as it reads a file.

use std::borrow::Cow;
use std::ops::Range;

use super::{CellId, Format, Kind, Notebook};

/// The code cells' sources in the notebook's order, a line break after each, so a cell is a run
/// of whole lines and a position maps between a cell and the program by the cell's first line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Program {
    pub text: String,
    cells: Vec<Span>,
}

/// Where a cell's lines are in [`Program::text`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct Span {
    id: CellId,
    first: u32,
    lines: u32,
    /// The last line holding code, neither blank nor a comment.
    last: u32,
}

/// The cell magics whose body is Python: the rest run another language, or none.
const PYTHON_BODIES: [&str; 3] = ["time", "timeit", "capture"];

impl Notebook {
    /// The code cells as one [`Program`]; an `.ipynb`'s Python with IPython's own syntax made
    /// Python, line for line with ordinary Python columns unchanged ([`as_python`]).
    pub fn program(&self) -> Program {
        let ipython = self.format() == Format::Ipynb && self.language == "python";
        let mut program = Program::default();
        for cell in self.cells.iter().filter(|c| c.kind == Kind::Code) {
            let source = match ipython {
                true => Cow::Owned(as_python(&cell.source)),
                false => Cow::Borrowed(cell.source.as_str()),
            };
            let first = program.cells.last().map_or(0, |s| s.first + s.lines);
            let lines: Vec<&str> = source.split('\n').collect();
            let last = lines.iter().rposition(|l| {
                let code = l.trim_start();
                !code.is_empty() && !code.starts_with('#')
            });
            program.cells.push(Span {
                id: cell.id,
                first,
                lines: lines.len() as u32,
                last: first + last.unwrap_or(0) as u32,
            });
            program.text.push_str(&source);
            program.text.push('\n');
        }
        program
    }
}

impl Program {
    /// The lines cell `id` is, `None` for a cell that is not code.
    pub fn lines(&self, id: CellId) -> Option<Range<u32>> {
        let span = self.cells.iter().find(|s| s.id == id)?;
        Some(span.first..span.first + span.lines)
    }

    /// The cell holding `line`, and which of its lines that is.
    pub fn cell_at(&self, line: u32) -> Option<(CellId, u32)> {
        let span = self.span_at(line)?;
        Some((span.id, line - span.first))
    }

    /// Whether `line` is its cell's last line of code, whose value a notebook shows.
    pub fn ends_cell(&self, line: u32) -> bool {
        self.span_at(line).is_some_and(|s| s.last == line)
    }

    fn span_at(&self, line: u32) -> Option<&Span> {
        let at = self
            .cells
            .partition_point(|s| s.first <= line)
            .checked_sub(1)?;
        let span = &self.cells[at];
        (line < span.first + span.lines).then_some(span)
    }
}

/// `source` with IPython commands made Python, keeping names and positions where possible.
/// A cell under any other cell magic is blank lines, its body being another language's.
fn as_python(source: &str) -> String {
    let magic = source
        .strip_prefix("%%")
        .map(|m| m.split_whitespace().next().unwrap_or_default());
    if magic.is_some_and(|m| !PYTHON_BODIES.contains(&m)) {
        return "\n".repeat(source.matches('\n').count());
    }
    let lines: Vec<Cow<str>> = source
        .split('\n')
        .map(|line| {
            let code = line.trim_start();
            if code.starts_with(['%', '!']) {
                let indent = &line[..line.len() - code.len()];
                let marker = if indent.is_empty() { "#" } else { "pass #" };
                return format!("{indent}{marker}{}", &code[1..]).into();
            }
            if let Some((lhs, rhs)) = line.split_once('=') {
                let command = rhs.trim_start();
                if is_python_name(lhs.trim()) && command.starts_with('!') {
                    let prefix = &line[..line.len() - command.len()];
                    return format!("{prefix}[]{}", " ".repeat(command.len().saturating_sub(2)))
                        .into();
                }
            }
            let trimmed = line.trim_end();
            if let Some(name) = trimmed.strip_suffix('?')
                && name.trim_start().split('.').all(is_python_name)
            {
                return format!("{name} {}", &line[trimmed.len()..]).into();
            }
            line.into()
        })
        .collect();
    lines.join("\n")
}

fn is_python_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notebook::{Cell, EMPTY_IPYNB, Edit, read};

    const ANALYSIS: &str = include_str!("../../tests/fixtures/notebook/analysis.ipynb");
    const EXPLORE: &str = include_str!("../../tests/fixtures/notebook/explore.py");

    #[test]
    fn the_code_cells_are_one_program_a_line_apart() {
        let nb = read("analysis.ipynb", ANALYSIS).unwrap();
        let program = nb.program();
        let code: Vec<&Cell> = nb
            .cells()
            .iter()
            .filter(|c| c.kind() == Kind::Code)
            .collect();
        let sources: Vec<&str> = code.iter().map(|c| c.source()).collect();
        assert_eq!(program.text, format!("{}\n", sources.join("\n")));
        let cells = nb.cells();
        assert_eq!(program.lines(cells[0].id()), None, "a markdown cell");
        assert_eq!(program.lines(cells[1].id()), Some(0..4), "four imports");
        assert_eq!(program.lines(cells[2].id()), Some(4..8));
        assert_eq!(program.cell_at(5), Some((cells[2].id(), 1)));
        // The last cell is empty, and one line all the same.
        let lines = program.text.lines().count() as u32;
        let last = code.last().unwrap().id();
        assert_eq!(program.cell_at(lines - 1), Some((last, 0)));
        assert_eq!(program.cell_at(lines), None);
    }

    #[test]
    fn ipython_magics_are_comments_and_cell_magics_blank() {
        let mut nb = read("a.ipynb", EMPTY_IPYNB).unwrap();
        let id = nb.cells()[0].id();
        let text = "%matplotlib inline\n!pip install numpy\nimport os".to_string();
        nb.apply(Edit::Source { id, text });
        for (at, source) in [
            (1, "%%bash\necho hi\nls"),
            (2, "%%timeit -n 3\nsum(range(9))"),
        ] {
            let cell = nb.new_cell(Kind::Code, source).unwrap();
            nb.apply(Edit::Insert { at, cell });
        }
        let program = nb.program();
        assert_eq!(
            program.text,
            "#matplotlib inline\n#pip install numpy\nimport os\n\n\n\n#%timeit -n 3\nsum(range(9))\n"
        );
        assert_eq!(program.lines(nb.cells()[2].id()), Some(6..8));
    }

    #[test]
    fn indented_ipython_lines_keep_their_python_blocks_nonempty() {
        assert_eq!(
            as_python("if ready:\n    %timeit work()\nelse:\n    !date\n"),
            "if ready:\n    pass #timeit work()\nelse:\n    pass #date\n"
        );
        assert_eq!(as_python("!date\n%timeit work()"), "#date\n#timeit work()");
    }

    #[test]
    fn shell_capture_and_inspection_keep_their_names_in_the_program() {
        let source = "x = !echo hi\nif x:\n    obj?\ntext = 'obj?'\nprint('x = !stay')";
        let python = as_python(source);
        assert_eq!(
            python,
            "x = []      \nif x:\n    obj \ntext = 'obj?'\nprint('x = !stay')"
        );
        for (before, after) in source.lines().zip(python.lines()) {
            assert_eq!(before.len(), after.len(), "{before:?}");
        }
    }

    #[test]
    fn a_marimo_program_is_its_cells_at_the_margin_and_ends_are_known() {
        let mut nb = read("explore.py", EXPLORE).unwrap();
        let program = nb.program();
        assert!(
            program
                .text
                .starts_with("import marimo as mo\nimport numpy as np\ndef fidelity(p01, p10):\n")
        );
        assert!(
            !program.text.contains("Explore the sweep"),
            "markdown is out"
        );
        assert!(
            program
                .text
                .contains("\namps = np.linspace(0.1, 1.0, 10)\n")
        );
        let line = |program: &Program, text: &str| {
            program.text.lines().position(|l| l == text).unwrap() as u32
        };
        let head = line(&program, "calibration_table_by_qubit.head()");
        assert!(program.ends_cell(head));
        assert!(!program.ends_cell(head - 1));
        // A comment and a blank line after a cell's value leave it its last line of code.
        let id = nb.cells()[8].id();
        let text = format!("{}\n# the slider\n", nb.cells()[8].source());
        nb.apply(Edit::Source { id, text });
        let program = nb.program();
        assert!(program.ends_cell(line(&program, "shots")));
    }
}
