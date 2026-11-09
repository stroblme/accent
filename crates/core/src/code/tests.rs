//! The `code` module's tests.

use super::*;

/// What a test compares: kind, name, container, first and last line, test.
type Row = (SymbolKind, String, Option<String>, u32, u32, bool);

fn rows(lang: Lang, text: &str) -> Vec<Row> {
    symbols(lang, text)
        .into_iter()
        .map(|d| (d.kind, d.name, d.container, d.line, d.end_line, d.test))
        .collect()
}

fn row(kind: SymbolKind, name: &str, container: Option<&str>, lines: (u32, u32)) -> Row {
    (
        kind,
        name.into(),
        container.map(Into::into),
        lines.0,
        lines.1,
        false,
    )
}

fn test(kind: SymbolKind, name: &str, container: Option<&str>, lines: (u32, u32)) -> Row {
    (
        kind,
        name.into(),
        container.map(Into::into),
        lines.0,
        lines.1,
        true,
    )
}

/// Every number below counts the lines of its fixture from 1.
#[test]
fn rust_items_and_their_containers() {
    let text = r##"//! A module { with braces in a comment
use std::fmt;

/// A doc { comment
#[derive(Debug)]
pub struct Point {
    x: i32,
}

pub(crate) struct Unit;
struct Tuple(u8, [u8; 4]);

pub enum Shape { Circle { r: f64 }, Square }

pub trait Area {
    fn area(&self) -> f64;
    fn twice(&self) -> f64 {
        self.area() * 2.0
    }
}

impl<T: Fn() -> u8> Area for Wrapper<T> where T: Clone {
    type Output = u8;
    const SIDES: u8 = 4;
    fn area(&self) -> f64 {
        let s = "}{ not code";
        let c = '{';
        let r = r#"fn fake() {"#;
        let l: &'static str = "x";
        0.0
    }
}

impl Point {
    pub const fn new(x: i32) -> Self {
        fn helper() {}
        Point { x }
    }
    async unsafe fn spin<'a>(&'a self) {}
}

pub type Pair<'a> = (&'a str, &'a str);
static NAMES: [&str; 2] = ["a", "b"];
macro_rules! square { ($x:expr) => { $x * $x }; }
extern "C" fn callback() {}
fn r#match() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {}
    #[tokio::test]
    async fn it_waits() {}
}
"##;
    assert_eq!(
        rows(Lang::Rust, text),
        [
            row(Struct, "Point", None, (6, 8)),
            row(Struct, "Unit", None, (10, 10)),
            row(Struct, "Tuple", None, (11, 11)),
            row(Enum, "Shape", None, (13, 13)),
            row(Trait, "Area", None, (15, 20)),
            row(Method, "area", Some("Area"), (16, 16)),
            row(Method, "twice", Some("Area"), (17, 19)),
            row(Impl, "Area", Some("Wrapper"), (22, 22)),
            row(TypeAlias, "Output", Some("Wrapper"), (23, 23)),
            row(Const, "SIDES", Some("Wrapper"), (24, 24)),
            row(Method, "area", Some("Wrapper"), (25, 31)),
            row(Method, "new", Some("Point"), (35, 38)),
            row(Function, "helper", None, (36, 36)),
            row(Method, "spin", Some("Point"), (39, 39)),
            row(TypeAlias, "Pair", None, (42, 42)),
            row(Const, "NAMES", None, (43, 43)),
            row(Macro, "square", None, (44, 44)),
            row(Function, "callback", None, (45, 45)),
            row(Function, "r#match", None, (46, 46)),
            test(SymbolKind::Module, "tests", None, (49, 56)),
            test(Function, "it_works", None, (53, 53)),
            test(Function, "it_waits", None, (55, 55)),
        ]
    );
    let new = &symbols(Lang::Rust, text)[11];
    assert_eq!(new.signature, "pub const fn new(x: i32) -> Self");
    assert!(text[new.range.clone()].starts_with("    pub const fn new"));
    assert!(text[new.range.clone()].ends_with("Point { x }\n    }"));
}

/// What a type implements or extends is a row each: a trait's supertraits, `impl Trait for`, a
/// class's `:`, `extends` and `implements`, a Python class's bases. A lifetime, a negative or
/// inherent `impl`, a `where` clause, a constructor's parameters and a C enum's stored type
/// are none.
#[test]
fn supertypes_are_rows_of_their_own() {
    let impls = |lang, text: &str| -> Vec<(String, String)> {
        symbols(lang, text)
            .into_iter()
            .filter(|d| d.kind == Impl)
            .map(|d| (d.container.unwrap_or_default(), d.name))
            .collect()
    };
    let pairs = |list: &[(&str, &str)]| -> Vec<(String, String)> {
        list.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    let rust = "trait Probe: Saves + Sized + 'static {}\nunsafe impl<T> Send for a::Tab<T> {}\n\
                impl !Sync for Tab {}\nimpl Tab {}\nimpl<'a> fmt::Display\n    for Tab {}\n\
                struct S<T> where T: Clone {}\n";
    assert_eq!(
        impls(Lang::Rust, rust),
        pairs(&[
            ("Probe", "Saves"),
            ("Probe", "Sized"),
            ("Tab", "Send"),
            ("Tab", "Display")
        ])
    );
    let kotlin =
        "data class A(val x: Int) : B(x), C by c\nclass D<T : Any> : E<T> where T : F {\n}\n";
    assert_eq!(
        impls(Lang::Kotlin, kotlin),
        pairs(&[("A", "B"), ("A", "C"), ("D", "E")])
    );
    let java = "public class A<T> extends B<T> implements C, D {\n}\ninterface E extends F, G {}\n";
    assert_eq!(
        impls(Lang::Java, java),
        pairs(&[("A", "B"), ("A", "C"), ("A", "D"), ("E", "F"), ("E", "G")])
    );
    let cpp =
        "class B final : public C, private virtual D {\n};\nenum class E : unsigned char {};\n";
    assert_eq!(impls(Lang::C, cpp), pairs(&[("B", "C"), ("B", "D")]));
    let python = "class A(b.B, Generic[T], metaclass=M):\n    pass\nclass C(object):\n    pass\n";
    assert_eq!(
        impls(Lang::Python, python),
        pairs(&[("A", "B"), ("A", "Generic")])
    );
}

/// Repository fixtures ensure every `fn` declaration is found despite strings, lifetimes and
/// doc comments.
#[test]
fn rust_files_of_this_repository() {
    let fns = Regex::new(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:(?:const|async|unsafe)[ \t]+)*fn[ \t]+(\w+)",
    )
    .unwrap();
    for text in [
        include_str!("../index/links.rs"),
        include_str!("../index/search.rs"),
        include_str!("../markdown/mod.rs"),
        include_str!("../code.rs"),
        include_str!("tests.rs"),
    ] {
        let found = symbols(Lang::Rust, text);
        let callables: Vec<&str> = found
            .iter()
            .filter(|d| d.kind.callable() && d.kind != Macro)
            .map(|d| d.name.as_str())
            .collect();
        let masked = mask(Lang::Rust, text);
        let lines: Vec<&str> = fns
            .captures_iter(&masked)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(callables, lines);
        for d in &found {
            let line = text.lines().nth(d.line as usize - 1).unwrap();
            assert!(line.contains(d.name.as_str()), "{d:?} on {line:?}");
        }
    }
    let links = symbols(Lang::Rust, include_str!("../index/links.rs"));
    let find = |name: &str| links.iter().find(|d| d.name == name).unwrap();
    assert_eq!(find("resolve_links_of").kind, Function);
    assert_eq!(find("backlinks").container.as_deref(), Some("Index"));
    assert!(find("links_from_lists_where_each_link_leads").test);
    assert!(!find("links_from").test);
}

#[test]
fn python_by_indentation() {
    let text = r#""""Module docstring with def fake(): and class Fake:"""
import os

CONST = "def not_a_function():"

def top(a,
        b):
    """Doc."""
    return a  # def comment():

class Thing(Base):
    attr = 1

    def method(self):
        def inner():
            pass
        return inner

    async def other(self) -> None: ...

    @property
    def prop(self):
        return 1

def test_thing():
    assert Thing()

class TestGroup:
    def check(self):
        pass
"#;
    assert_eq!(
        rows(Lang::Python, text),
        [
            row(Function, "top", None, (6, 9)),
            row(Class, "Thing", None, (11, 23)),
            row(Impl, "Base", Some("Thing"), (11, 11)),
            row(Method, "method", Some("Thing"), (14, 17)),
            row(Function, "inner", None, (15, 16)),
            row(Method, "other", Some("Thing"), (19, 19)),
            row(Method, "prop", Some("Thing"), (22, 23)),
            test(Function, "test_thing", None, (25, 26)),
            test(Class, "TestGroup", None, (28, 30)),
            test(Method, "check", Some("TestGroup"), (29, 30)),
        ]
    );
    // As codegraph's tree-sitter parse has them.
    let xtest = symbols(Lang::Python, include_str!("../../../../build-aux/xtest.py"));
    let lines: Vec<(&str, u32, u32)> = xtest
        .iter()
        .map(|d| (d.name.as_str(), d.line, d.end_line))
        .collect();
    assert_eq!(
        lines,
        [
            ("move", 22, 23),
            ("keycode", 24, 28),
            ("key", 29, 33),
            ("focus", 34, 42),
            ("button", 43, 44)
        ]
    );
}

/// A declaration starts at its annotations, as Kotlin's and Java's grammars have it.
#[test]
fn kotlin_classes_objects_and_expression_bodies() {
    let text = r#"package a.b

import x.y

/* class Fake { */
data class Point(val x: Int, val y: Int)

interface Shape {
    fun area(): Double
    fun name() = "shape {"
}

class Circle(private val r: Double) : Shape {
    override fun area(): Double {
        val s = "}"
        return 3.14 * r * r
    }

    companion object {
        fun unit() = Circle(1.0)
    }
}

enum class Color { RED, GREEN }

object Registry {
    fun register(s: Shape) {}
}

fun <T> List<T>.second(): T = this[1]

typealias Shapes = List<Shape>

class CircleTest {
    @Test
    fun `area is pi r squared`() {
        assertEquals(3.14, Circle(1.0).area(), 0.01)
    }
}
"#;
    assert_eq!(
        rows(Lang::Kotlin, text),
        [
            row(Class, "Point", None, (6, 6)),
            row(Trait, "Shape", None, (8, 11)),
            row(Method, "area", Some("Shape"), (9, 9)),
            row(Method, "name", Some("Shape"), (10, 10)),
            row(Class, "Circle", None, (13, 22)),
            row(Impl, "Shape", Some("Circle"), (13, 13)),
            row(Method, "area", Some("Circle"), (14, 17)),
            row(Method, "unit", Some("Circle"), (20, 20)),
            row(Enum, "Color", None, (24, 24)),
            row(Class, "Registry", None, (26, 28)),
            row(Method, "register", Some("Registry"), (27, 27)),
            row(Function, "second", None, (30, 30)),
            row(TypeAlias, "Shapes", None, (32, 32)),
            row(Class, "CircleTest", None, (34, 39)),
            test(Method, "area is pi r squared", Some("CircleTest"), (35, 38)),
        ]
    );
}

#[test]
fn java_members_but_not_calls_or_enum_constants() {
    let text = r#"package a;

import java.util.List;

/** Doc { */
public class Shapes {
    private final List<String> names = List.of("a{");

    public Shapes() {
        super();
    }

    public static <T> T first(List<T> list) {
        return list.get(0);
    }

    @Override
    public String toString() {
        return "Shapes";
    }

    interface Visitor {
        void visit(Shapes s);
    }

    enum Kind {
        ROUND("r"), SQUARE("s");
        Kind(String code) {}
    }

    abstract static class Base {
        abstract int size();
    }
}
"#;
    assert_eq!(
        rows(Lang::Java, text),
        [
            row(Class, "Shapes", None, (6, 34)),
            row(Method, "Shapes", Some("Shapes"), (9, 11)),
            row(Method, "first", Some("Shapes"), (13, 15)),
            row(Method, "toString", Some("Shapes"), (17, 20)),
            row(Trait, "Visitor", Some("Shapes"), (22, 24)),
            row(Method, "visit", Some("Visitor"), (23, 23)),
            row(Enum, "Kind", Some("Shapes"), (26, 29)),
            row(Method, "Kind", Some("Kind"), (28, 28)),
            row(Class, "Base", Some("Shapes"), (31, 33)),
            row(Method, "size", Some("Base"), (32, 32)),
        ]
    );
}

#[test]
fn script_functions_classes_and_literals() {
    let text = r#"// function fake() {
import { x } from "y";

export function add(a: number, b: number): number {
  return a + b;
}

export default async function* gen() {}

const re = /\{[}]/g;
const tpl = `${a} { }`;

export const mul = (a: number, b: number) => {
  return a * b;
};

export class Box<T> extends Base {
  #secret = 1;
  constructor(private v: T) {
    super();
  }
  get value(): T {
    return this.v;
  }
  static of<T>(v: T): Box<T> {
    if (v) {
      return new Box(v);
    }
  }
}

export interface Shape {
  area(): number;
}

export type Pair = [number, number];

enum Color { Red, Green }

describe("Box", () => {
  it("holds", () => {});
});
"#;
    assert_eq!(
        rows(Lang::Script, text),
        [
            row(Function, "add", None, (4, 6)),
            row(Function, "gen", None, (8, 8)),
            row(Function, "mul", None, (13, 15)),
            row(Class, "Box", None, (17, 30)),
            row(Impl, "Base", Some("Box"), (17, 17)),
            row(Method, "constructor", Some("Box"), (19, 21)),
            row(Method, "value", Some("Box"), (22, 24)),
            row(Method, "of", Some("Box"), (25, 29)),
            row(Trait, "Shape", None, (32, 34)),
            row(Method, "area", Some("Shape"), (33, 33)),
            row(TypeAlias, "Pair", None, (36, 36)),
            row(Enum, "Color", None, (38, 38)),
        ]
    );
    let minified = format!("function a(){{}}{}", " ".repeat(MINIFIED));
    assert!(symbols(Lang::Script, &minified).is_empty());
}

#[test]
fn go_types_functions_and_receivers() {
    let text = r#"package main

import "fmt"

// func fake() {
type Point struct {
	X, Y int
}

type Shape interface {
	Area() float64
}

type ID int

type Alias = Point

func (p *Point) Area() float64 {
	s := "}"
	r := '{'
	raw := `{`
	return 0
}

func main() {
	fmt.Println("hi")
}

func TestMain(t *testing.T) {}
"#;
    assert_eq!(
        rows(Lang::Go, text),
        [
            row(Struct, "Point", None, (6, 8)),
            row(Trait, "Shape", None, (10, 12)),
            row(TypeAlias, "ID", None, (14, 14)),
            row(TypeAlias, "Alias", None, (16, 16)),
            row(Method, "Area", Some("Point"), (18, 23)),
            row(Function, "main", None, (25, 27)),
            test(Function, "TestMain", None, (29, 29)),
        ]
    );
}

#[test]
fn c_definitions_not_calls_or_prototypes() {
    let text = r#"#include <stdio.h>
#define MAX(a, b) ((a) > (b) ? (a) : (b))

/* int fake(void) { */
struct point {
    int x, y;
};

typedef struct node {
    struct node *next;
} node_t;

enum color { RED, GREEN };

static int
add(int a, int b)
{
    const char *s = "}";
    char c = '{';
    if (a > b) {
        return a;
    }
    return a + b;
}

int main(void) {
    for (int i = 0; i < 3; i++) {
        printf("%d\n", add(i, 1));
    }
    return 0;
}

namespace geo {
class Shape : public Base {
public:
    virtual double area() const = 0;
    double twice() const { return 2 * area(); }
};
}

double geo::Shape::half() const {
    return area() / 2;
}
"#;
    assert_eq!(
        rows(Lang::C, text),
        [
            row(Macro, "MAX", None, (2, 2)),
            row(Struct, "point", None, (5, 7)),
            row(Struct, "node", None, (9, 11)),
            row(Enum, "color", None, (13, 13)),
            row(Function, "add", None, (16, 24)),
            row(Function, "main", None, (26, 31)),
            row(Class, "Shape", None, (34, 38)),
            row(Impl, "Base", Some("Shape"), (34, 34)),
            row(Method, "twice", Some("Shape"), (37, 37)),
            row(Method, "half", Some("Shape"), (41, 43)),
        ]
    );
}

#[test]
fn calls_are_names_before_a_parenthesis() {
    let body = mask(
        Lang::Rust,
        "{ let x = foo(1); bar::<u8>(x); vec![]; println!(\"baz(\"); foo(2) }",
    );
    assert_eq!(calls(&body), ["foo", "bar", "println"]);
}

#[test]
fn a_name_is_found_as_a_word_in_its_declaration() {
    let text =
        "impl A {\n    #[inline]\n    pub fn renew(&self) -> New { new() }\n    fn new() {}\n}\n";
    let decl = |line: &str| {
        let start = text.find(line).unwrap();
        start..start + line.len()
    };
    let at = name_at(
        text,
        decl("    pub fn renew(&self) -> New { new() }"),
        "new",
    );
    assert_eq!(at, Some(text.find("new()").unwrap()));
    assert_eq!(
        name_at(text, decl("    fn new() {}"), "new"),
        Some(text.find("fn new").unwrap() + 3)
    );
    assert_eq!(name_at(text, decl("    #[inline]"), "new"), None);
}

#[test]
fn languages_and_test_files_by_path() {
    assert_eq!(lang_of("src/main.rs"), Some(Lang::Rust));
    assert_eq!(lang_of("Math/Algebra.lean"), Some(Lang::Lean));
    assert_eq!(lang_of("thesis/main.typ"), Some(Lang::Typst));
    assert_eq!(lang_of("app/x.TSX"), Some(Lang::Script));
    assert_eq!(lang_of("notes/a.md"), None);
    assert_eq!(lang_of("Makefile"), None);
    assert!(is_test_path("apps/cli/tests/mcp.rs"));
    assert!(is_test_path("crates/core/src/git/tests.rs"));
    assert!(is_test_path("pkg/x_test.go"));
    assert!(is_test_path("tests/test_api.py"));
    assert!(is_test_path("src/box.spec.ts"));
    assert!(!is_test_path("crates/core/src/index/links.rs"));
}

#[test]
fn lean_declarations_nest_in_namespaces_and_ignore_comments() {
    let text = r#"-- def fake := 0
/- outer
   /- nested -/
   theorem fake : True := by trivial
-/
namespace Algebra
section Helpers
private def add' (x : Nat) := x + 1
end Helpers
namespace Inner
@[simp] theorem φ' : True := by
  trivial
end Inner
end Algebra
def outside := "theorem NotReal"
def quoted := "start
theorem NotReal : True := by trivial
end"
"#;
    assert_eq!(
        rows(Lang::Lean, text),
        [
            row(Module, "Algebra", None, (6, 14)),
            row(Function, "add'", Some("Algebra"), (8, 8)),
            row(Module, "Inner", Some("Algebra"), (10, 13)),
            row(Function, "φ'", Some("Inner"), (11, 11)),
            row(Function, "outside", None, (15, 15)),
            row(Function, "quoted", None, (16, 16)),
        ]
    );
}
