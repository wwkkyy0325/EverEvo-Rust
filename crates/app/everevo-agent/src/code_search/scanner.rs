//! Code scanner — regex-based symbol extraction for Rust, Python, JS/TS, Go.
//!
//! No Tree-sitter dependency. Uses regex patterns per language.
//! Research shows line-based + regex is a strong baseline for code retrieval
//! (JetBrains 2025: line-based ≈ AST-aware across budgets).

use regex::Regex;
use std::path::Path;

/// A discovered code symbol.
#[derive(Debug, Clone)]
pub struct CodeSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line: usize,
    pub parent: String,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Impl,
    Trait,
    Enum,
    Module,
    TypeAlias,
    Constant,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Struct => "struct",
            SymbolKind::Impl => "impl",
            SymbolKind::Trait => "trait",
            SymbolKind::Enum => "enum",
            SymbolKind::Module => "mod",
            SymbolKind::TypeAlias => "type",
            SymbolKind::Constant => "const",
        }
    }
}

/// Language patterns for symbol extraction.
struct LangPatterns {
    functions: Regex,
    structs: Regex,
    impls: Option<Regex>,
    traits: Option<Regex>,
    enums: Option<Regex>,
    modules: Regex,
    types: Option<Regex>,
    constants: Regex,
    #[allow(dead_code)]
    extensions: Vec<&'static str>,
}

fn rust_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(
            r"^\s*(?:pub(?:\s*\(\s*crate\s*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)",
        )
        .unwrap(),
        structs: Regex::new(r"^\s*(?:pub(?:\s*\(\s*crate\s*\))?\s+)?struct\s+(\w+)").unwrap(),
        impls: Some(Regex::new(r"^\s*impl\s+(?:[^<]*?)(?:<[^>]*>)?\s*(\w+)").unwrap()),
        traits: Some(
            Regex::new(r"^\s*(?:pub(?:\s*\(\s*crate\s*\))?\s+)?(?:unsafe\s+)?trait\s+(\w+)")
                .unwrap(),
        ),
        enums: Some(Regex::new(r"^\s*(?:pub(?:\s*\(\s*crate\s*\))?\s+)?enum\s+(\w+)").unwrap()),
        modules: Regex::new(r"^\s*(?:pub\s+)?mod\s+(\w+)").unwrap(),
        types: Some(Regex::new(r"^\s*(?:pub(?:\s*\(\s*crate\s*\))?\s+)?type\s+(\w+)").unwrap()),
        constants: Regex::new(r"^\s*(?:pub(?:\s*\(\s*crate\s*\))?\s+)?const\s+(\w+)").unwrap(),
        extensions: vec!["rs"],
    }
}

fn python_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(r"^\s*def\s+(\w+)").unwrap(),
        structs: Regex::new(r"^\s*class\s+(\w+)").unwrap(),
        impls: None,
        traits: None,
        enums: None,
        modules: Regex::new(r"^\s*(?:from\s+(\S+)\s+)?import\s+(\S+)").unwrap(),
        types: None,
        constants: Regex::new(r"^\s*([A-Z][A-Z_0-9]+)\s*=").unwrap(),
        extensions: vec!["py"],
    }
}

fn js_ts_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(r"(?:^\s*(?:export\s+)?(?:async\s+)?function\s+(\w+)|^\s*(?:export\s+)?(?:const|let|var)\s+(\w+)\s*=\s*(?:async\s+)?\()").unwrap(),
        structs: Regex::new(r"^\s*(?:export\s+)?(?:class|interface)\s+(\w+)").unwrap(),
        impls: None,
        traits: None,
        enums: None,
        modules: Regex::new(r"^\s*import\s").unwrap(),
        types: Some(Regex::new(r"^\s*(?:export\s+)?(?:type|interface)\s+(\w+)").unwrap()),
        constants: Regex::new(r"^\s*(?:export\s+)?const\s+([A-Z][A-Z_0-9]*)\s*=").unwrap(),
        extensions: vec!["ts", "tsx", "js", "jsx", "mjs", "cjs"],
    }
}

fn go_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(r"^\s*func\s+(?:\(\s*\w+\s+\*?\w+\s*\)\s+)?(\w+)").unwrap(),
        structs: Regex::new(r"^\s*type\s+(\w+)\s+struct").unwrap(),
        impls: None,
        traits: None,
        enums: None,
        modules: Regex::new(r#"^\s*import\s+"#).unwrap(),
        types: Some(Regex::new(r"^\s*type\s+(\w+)\s+(?:interface|func|[^s])").unwrap()),
        constants: Regex::new(r"^\s*const\s+(?:\(\s*)?(\w*)").unwrap(),
        extensions: vec!["go"],
    }
}

fn java_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(r"^\s*(?:public|private|protected)?\s*(?:static\s+)?(?:final\s+)?(?:<[^>]*>\s+)?\w+(?:<[^>]*>)?\s+(\w+)\s*\(").unwrap(),
        structs: Regex::new(r"^\s*(?:public\s+)?(?:abstract\s+)?(?:final\s+)?class\s+(\w+)").unwrap(),
        impls: None,
        traits: Some(Regex::new(r"^\s*(?:public\s+)?interface\s+(\w+)").unwrap()),
        enums: Some(Regex::new(r"^\s*(?:public\s+)?enum\s+(\w+)").unwrap()),
        modules: Regex::new(r"^\s*package\s+(\S+)").unwrap(),
        types: None,
        constants: Regex::new(r"^\s*(?:public\s+)?(?:static\s+)?(?:final\s+)?\w+\s+([A-Z][A-Z_0-9]*)\s*=").unwrap(),
        extensions: vec!["java"],
    }
}

fn c_cpp_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(r"^\s*(?:inline\s+)?(?:static\s+)?(?:const\s+)?(?:virtual\s+)?\w+(?:\s*\*)?\s+(\w+)\s*\([^)]*\)\s*(?:const\s*)?\{?").unwrap(),
        structs: Regex::new(r"^\s*(?:class|struct)\s+(\w+)").unwrap(),
        impls: None,
        traits: None,
        enums: Some(Regex::new(r"^\s*enum\s+(?:class\s+)?(\w+)").unwrap()),
        modules: Regex::new(r#"^\s*#include\s"#).unwrap(),
        types: Some(Regex::new(r"^\s*typedef\s+.+\s+(\w+)\s*;").unwrap()),
        constants: Regex::new(r"^\s*#define\s+([A-Z][A-Z_0-9]*)").unwrap(),
        extensions: vec!["c", "cpp", "h", "hpp", "cc", "cxx"],
    }
}

fn kotlin_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(
            r"^\s*(?:suspend\s+)?(?:private|internal|protected|public)?\s*fun\s+(\w+)\s*\(",
        )
        .unwrap(),
        structs: Regex::new(
            r"^\s*(?:data\s+)?(?:sealed\s+)?(?:abstract\s+)?(?:open\s+)?class\s+(\w+)",
        )
        .unwrap(),
        impls: None,
        traits: Some(Regex::new(r"^\s*interface\s+(\w+)").unwrap()),
        enums: Some(Regex::new(r"^\s*enum\s+class\s+(\w+)").unwrap()),
        modules: Regex::new(r"^\s*package\s+(\S+)").unwrap(),
        types: Some(Regex::new(r"^\s*typealias\s+(\w+)").unwrap()),
        constants: Regex::new(r"^\s*(?:const\s+)?val\s+([A-Z][A-Z_0-9]*)\s*=").unwrap(),
        extensions: vec!["kt", "kts"],
    }
}

fn ruby_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(r"^\s*def\s+(?:self\.)?(\w+)").unwrap(),
        structs: Regex::new(r"^\s*class\s+(\w+)").unwrap(),
        impls: None,
        traits: None,
        enums: None,
        modules: Regex::new(r"^\s*module\s+(\w+)").unwrap(),
        types: None,
        constants: Regex::new(r"^\s*([A-Z][A-Z_0-9]*)\s*=").unwrap(),
        extensions: vec!["rb"],
    }
}

fn swift_patterns() -> LangPatterns {
    LangPatterns {
        functions: Regex::new(r"^\s*(?:private|public|internal|fileprivate|open)?\s*(?:static\s+)?(?:override\s+)?func\s+(\w+)\s*\(").unwrap(),
        structs: Regex::new(r"^\s*(?:public\s+)?(?:final\s+)?class\s+(\w+)").unwrap(),
        impls: None,
        traits: Some(Regex::new(r"^\s*protocol\s+(\w+)").unwrap()),
        enums: Some(Regex::new(r"^\s*(?:public\s+)?enum\s+(\w+)").unwrap()),
        modules: Regex::new(r"^\s*import\s+(\w+)").unwrap(),
        types: Some(Regex::new(r"^\s*typealias\s+(\w+)").unwrap()),
        constants: Regex::new(r"^\s*(?:static\s+)?(?:let|var)\s+([A-Z][A-Z_0-9]*)\s*:").unwrap(),
        extensions: vec!["swift"],
    }
}

/// Check if a line is a comment or string literal (should be skipped).
fn is_comment_or_string(line: &str, ext: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return true;
    }
    match ext {
        "rs" | "go" | "java" | "kt" | "kts" | "swift" | "c" | "cpp" | "h" | "hpp" | "cc"
        | "cxx" => {
            trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
                || trimmed.starts_with("///")
        }
        "py" => {
            trimmed.starts_with('#') || trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''")
        }
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*')
        }
        "rb" => trimmed.starts_with('#') || trimmed.starts_with("=begin"),
        _ => false,
    }
}

/// Scan a single file for code symbols. Returns empty vec for unsupported languages.
/// Now supports 9 languages: Rust, Python, JS/TS, Go, Java, C/C++, Kotlin, Ruby, Swift.
pub fn scan_file(path: &Path, relative_to: &Path) -> Vec<CodeSymbol> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let patterns = match ext {
        "rs" => rust_patterns(),
        "py" => python_patterns(),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => js_ts_patterns(),
        "go" => go_patterns(),
        "java" => java_patterns(),
        "c" | "cpp" | "h" | "hpp" | "cc" | "cxx" => c_cpp_patterns(),
        "kt" | "kts" => kotlin_patterns(),
        "rb" => ruby_patterns(),
        "swift" => swift_patterns(),
        _ => return Vec::new(),
    };

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let rel_path = path
        .strip_prefix(relative_to)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    let mut symbols = Vec::new();
    let mut parent = String::new();

    let rust_mod_re = Regex::new(r"^\s*(?:pub\s+)?mod\s+(\w+)").unwrap();

    for (line_num, line) in content.lines().enumerate() {
        let ln = line_num + 1;

        // Skip comment/string lines — prevents false matches on commented-out code
        if is_comment_or_string(line, ext) {
            continue;
        }

        // Track parent module
        if let Some(caps) = patterns.modules.captures(line) {
            let name = caps
                .get(1)
                .or_else(|| caps.get(2))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            if !name.is_empty() && ext == "rs" {
                if parent.is_empty() {
                    parent = name.clone();
                } else {
                    parent = format!("{}::{}", parent, name);
                }
            }
        }

        // Extract symbols
        macro_rules! try_extract {
            ($regex:expr, $kind:expr) => {
                if let Some(caps) = $regex.captures(line) {
                    let name = caps
                        .get(1)
                        .or_else(|| caps.get(2))
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default();
                    if !name.is_empty() && !is_keyword(&name) {
                        symbols.push(CodeSymbol {
                            name,
                            kind: $kind,
                            file: rel_path.clone(),
                            line: ln,
                            parent: parent.clone(),
                            signature: line.trim().to_string(),
                        });
                    }
                }
            };
        }

        try_extract!(patterns.functions, SymbolKind::Function);
        try_extract!(patterns.structs, SymbolKind::Struct);
        if let Some(ref rx) = patterns.impls {
            try_extract!(*rx, SymbolKind::Impl);
        }
        if let Some(ref rx) = patterns.traits {
            try_extract!(*rx, SymbolKind::Trait);
        }
        if let Some(ref rx) = patterns.enums {
            try_extract!(*rx, SymbolKind::Enum);
        }
        if let Some(ref rx) = patterns.types {
            try_extract!(*rx, SymbolKind::TypeAlias);
        }
        try_extract!(patterns.constants, SymbolKind::Constant);

        // Module symbols (Rust mod declarations)
        if ext == "rs" {
            if let Some(caps) = rust_mod_re.captures(line) {
                let name = caps.get(1).unwrap().as_str().to_string();
                if !name.is_empty() {
                    symbols.push(CodeSymbol {
                        name,
                        kind: SymbolKind::Module,
                        file: rel_path.clone(),
                        line: ln,
                        parent: parent.clone(),
                        signature: line.trim().to_string(),
                    });
                }
            }
        }
    }

    symbols
}

fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "pub"
            | "crate"
            | "self"
            | "super"
            | "mut"
            | "ref"
            | "true"
            | "false"
            | "if"
            | "else"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "return"
            | "let"
            | "use"
            | "as"
            | "in"
            | "where"
            | "async"
            | "await"
            | "move"
            | "dyn"
            | "impl"
            | "fn"
            | "mod"
            | "struct"
            | "enum"
            | "trait"
            | "type"
            | "const"
            | "static"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write_temp(content: &str, ext: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("test.{ext}"));
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn test_scan_rust_functions() {
        let code = "pub fn hello_world() {}\nfn private_fn() {}\npub async fn async_fn() {}";
        let (_dir, path) = write_temp(code, "rs");
        let symbols = scan_file(&path, _dir.path());
        let names: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .map(|s| s.name.clone())
            .collect();
        assert!(names.contains(&"hello_world".into()));
        assert!(names.contains(&"private_fn".into()));
        assert!(names.contains(&"async_fn".into()));
    }

    #[test]
    fn test_scan_rust_structs() {
        let code = "pub struct MyStruct {}\nstruct PrivateStruct;";
        let (_dir, path) = write_temp(code, "rs");
        let symbols = scan_file(&path, _dir.path());
        let names: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Struct)
            .map(|s| s.name.clone())
            .collect();
        assert!(names.contains(&"MyStruct".into()));
        assert!(names.contains(&"PrivateStruct".into()));
    }

    #[test]
    fn test_scan_python() {
        let code = "def my_func():\n    pass\n\nclass MyClass:\n    pass";
        let (_dir, path) = write_temp(code, "py");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols
            .iter()
            .any(|s| s.name == "my_func" && s.kind == SymbolKind::Function));
        assert!(symbols
            .iter()
            .any(|s| s.name == "MyClass" && s.kind == SymbolKind::Struct));
    }

    #[test]
    fn test_unsupported_extension() {
        let (_dir, path) = write_temp("hello", "txt");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols.is_empty());
    }

    // ── Comment filtering tests ──────────────────────────────────────

    #[test]
    fn test_comment_line_skipped_rust() {
        let code = "// fn commented_function() -> bool {}\npub fn real_function() {}";
        let (_dir, path) = write_temp(code, "rs");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols.iter().any(|s| s.name == "real_function"));
        assert!(!symbols.iter().any(|s| s.name == "commented_function"));
    }

    #[test]
    fn test_comment_line_skipped_python() {
        let code = "# def commented_func(): pass\ndef real_func():\n    pass";
        let (_dir, path) = write_temp(code, "py");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols.iter().any(|s| s.name == "real_func"));
        assert!(!symbols.iter().any(|s| s.name == "commented_func"));
    }

    #[test]
    fn test_doc_comment_skipped() {
        let code = "/// This is a doc comment\n/// fn fake_doc_fn()\npub fn real_one() {}";
        let (_dir, path) = write_temp(code, "rs");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols.iter().any(|s| s.name == "real_one"));
        assert!(!symbols.iter().any(|s| s.name == "fake_doc_fn"));
    }

    // ── New language tests ───────────────────────────────────────────

    #[test]
    fn test_scan_java() {
        let code = "public class UserService {\n    public User findUser(String id) {\n        return null;\n    }\n}";
        let (_dir, path) = write_temp(code, "java");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols
            .iter()
            .any(|s| s.name == "UserService" && s.kind == SymbolKind::Struct));
        assert!(symbols
            .iter()
            .any(|s| s.name == "findUser" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_scan_cpp() {
        let code = "class Calculator {\npublic:\n    int add(int a, int b);\n};";
        let (_dir, path) = write_temp(code, "cpp");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols
            .iter()
            .any(|s| s.name == "Calculator" && s.kind == SymbolKind::Struct));
    }

    #[test]
    fn test_scan_kotlin() {
        let code = "data class User(val name: String)\nfun main() {\n    println(\"hello\")\n}";
        let (_dir, path) = write_temp(code, "kt");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols
            .iter()
            .any(|s| s.name == "User" && s.kind == SymbolKind::Struct));
        assert!(symbols
            .iter()
            .any(|s| s.name == "main" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_scan_ruby() {
        let code = "class MyClass\n  def my_method\n    puts 'hello'\n  end\nend";
        let (_dir, path) = write_temp(code, "rb");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols
            .iter()
            .any(|s| s.name == "MyClass" && s.kind == SymbolKind::Struct));
        assert!(symbols
            .iter()
            .any(|s| s.name == "my_method" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_scan_swift() {
        let code = "class ViewController: UIViewController {\n    func viewDidLoad() {\n        super.viewDidLoad()\n    }\n}";
        let (_dir, path) = write_temp(code, "swift");
        let symbols = scan_file(&path, _dir.path());
        assert!(symbols
            .iter()
            .any(|s| s.name == "ViewController" && s.kind == SymbolKind::Struct));
        assert!(symbols
            .iter()
            .any(|s| s.name == "viewDidLoad" && s.kind == SymbolKind::Function));
    }
}
