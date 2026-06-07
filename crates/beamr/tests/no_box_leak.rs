//! CI gate preventing production `Box::leak` regressions.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct LeakMatch {
    path: PathBuf,
    line_number: usize,
    line: String,
}

#[test]
fn production_source_contains_no_box_leak_outside_atom_interning() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut disallowed = Vec::new();
    scan_rust_files(&src, &mut |path| {
        disallowed.extend(disallowed_box_leaks(path));
    });

    assert!(
        disallowed.is_empty(),
        "production Box::leak sites are not allowed outside src/atom/table.rs or test code:\n{}",
        format_matches(&disallowed)
    );
}

fn scan_rust_files(path: &Path, visit: &mut impl FnMut(&Path)) {
    let metadata = fs::metadata(path).expect("source path metadata");
    if metadata.is_dir() {
        let entries = fs::read_dir(path).expect("read source directory");
        for entry in entries {
            let entry = entry.expect("read source directory entry");
            scan_rust_files(&entry.path(), visit);
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        visit(path);
    }
}

fn disallowed_box_leaks(path: &Path) -> Vec<LeakMatch> {
    let source = fs::read_to_string(path).expect("read rust source file");
    let mut disallowed = Vec::new();
    let mut cfg_test_blocks = CfgTestBlocks::default();

    for (line_index, line) in source.lines().enumerate() {
        cfg_test_blocks.observe_line(line);
        if line.contains("Box::leak") && !is_allowed_leak(path, cfg_test_blocks.in_cfg_test_block())
        {
            disallowed.push(LeakMatch {
                path: path.to_path_buf(),
                line_number: line_index + 1,
                line: line.trim().to_string(),
            });
        }
    }

    disallowed
}

fn is_allowed_leak(path: &Path, in_cfg_test_block: bool) -> bool {
    is_atom_table(path) || is_test_source(path) || in_cfg_test_block
}

fn is_atom_table(path: &Path) -> bool {
    path.ends_with(Path::new("atom").join("table.rs"))
}

fn is_test_source(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("_tests.rs") || name == "tests.rs")
        || path.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| name == "tests")
        })
}

fn format_matches(matches: &[LeakMatch]) -> String {
    matches
        .iter()
        .map(|leak| {
            format!(
                "{}:{}: {}",
                leak.path.display(),
                leak.line_number,
                leak.line
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Default)]
struct CfgTestBlocks {
    pending_cfg_test: bool,
    active_depths: Vec<usize>,
    brace_depth: usize,
}

impl CfgTestBlocks {
    fn observe_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if starts_cfg_test_attribute(trimmed) {
            self.pending_cfg_test = true;
        }

        let opens = line.chars().filter(|character| *character == '{').count();
        let closes = line.chars().filter(|character| *character == '}').count();
        if self.pending_cfg_test && opens > 0 {
            self.active_depths.push(self.brace_depth + 1);
            self.pending_cfg_test = false;
        }

        self.brace_depth = self.brace_depth.saturating_add(opens);
        self.brace_depth = self.brace_depth.saturating_sub(closes);
        self.active_depths
            .retain(|depth| self.brace_depth >= *depth);
    }

    fn in_cfg_test_block(&self) -> bool {
        self.pending_cfg_test || !self.active_depths.is_empty()
    }
}

fn starts_cfg_test_attribute(line: &str) -> bool {
    line.starts_with("#[cfg(test)]")
        || line.starts_with("#[cfg(any(test")
        || line.starts_with("#[cfg(all(test")
}

#[cfg(test)]
mod tests {
    use super::CfgTestBlocks;

    #[test]
    fn cfg_test_blocks_cover_inline_attribute_items() {
        let mut blocks = CfgTestBlocks::default();
        blocks.observe_line("#[cfg(test)] fn helper() -> &'static str {");
        assert!(blocks.in_cfg_test_block());
        blocks.observe_line("Box::leak(String::new().into_boxed_str())");
        assert!(blocks.in_cfg_test_block());
        blocks.observe_line("}");
        assert!(!blocks.in_cfg_test_block());
    }

    #[test]
    fn cfg_test_blocks_cover_standard_test_modules() {
        let mut blocks = CfgTestBlocks::default();
        blocks.observe_line("#[cfg(test)]");
        assert!(blocks.in_cfg_test_block());
        blocks.observe_line("mod tests {");
        assert!(blocks.in_cfg_test_block());
        blocks.observe_line("}");
        assert!(!blocks.in_cfg_test_block());
    }
}
