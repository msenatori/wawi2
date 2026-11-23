//! A small TOML reader: just what `mascota.toml` needs, with no dependency.
//!
//! Supported: comments, `[table]` and `[dotted.table]` headers, bare and
//! quoted keys (dotted too), basic `"…"` and literal `'…'` strings and their
//! `"""…"""` / `'''…'''` multi-line forms, integers, floats, booleans, arrays
//! (over several lines, trailing comma allowed) and inline tables
//! `{ a = 1, b = "x" }`. Not supported: dates and arrays of tables
//! (`[[…]]`); both are reported as errors, never misread.
//!
//! Errors are in Spanish and carry the line: they are shown to artists.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Table),
}

/// Keys in file order (tables are small: a linear lookup is fine).
pub type Table = Vec<(String, Value)>;

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Integers count as numbers too (`fps = 8` and `fps = 8.0`).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    /// What it is, in the words of an error message.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Str(_) => "un texto",
            Value::Int(_) => "un número entero",
            Value::Float(_) => "un número",
            Value::Bool(_) => "un sí/no (true/false)",
            Value::Array(_) => "una lista",
            Value::Table(_) => "una tabla",
        }
    }
}

pub fn get<'a>(table: &'a Table, key: &str) -> Option<&'a Value> {
    table.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "línea {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

pub fn parse(text: &str) -> Result<Table, ParseError> {
    let mut p = Parser {
        chars: text.chars().collect(),
        i: 0,
        line: 1,
    };
    let mut root: Table = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut headers: Vec<Vec<String>> = Vec::new();
    loop {
        p.skip_blank_lines();
        let Some(c) = p.peek() else { break };
        if c == '[' {
            p.bump();
            if p.peek() == Some('[') {
                return Err(p.error("no se admiten tablas repetidas `[[...]]`"));
            }
            p.skip_spaces();
            let path = p.key_path()?;
            p.skip_spaces();
            if p.peek() != Some(']') {
                return Err(p.error("falta el `]` que cierra el nombre de la tabla"));
            }
            p.bump();
            p.end_of_line()?;
            if headers.contains(&path) {
                return Err(p.error(&format!("la tabla [{}] aparece dos veces", path.join("."))));
            }
            table_at(&mut root, &path).map_err(|m| p.error(&m))?;
            headers.push(path.clone());
            current = path;
        } else {
            let path = p.key_path()?;
            p.skip_spaces();
            if p.peek() != Some('=') {
                return Err(p.error(&format!(
                    "después de `{}` va un `=` y el valor",
                    path.join(".")
                )));
            }
            p.bump();
            p.skip_spaces();
            let value = p.value()?;
            p.end_of_line()?;
            let mut full = current.clone();
            full.extend(path);
            insert(&mut root, &full, value).map_err(|m| p.error(&m))?;
        }
    }
    Ok(root)
}

/// The table at `path`, created on the way.
fn table_at<'a>(root: &'a mut Table, path: &[String]) -> Result<&'a mut Table, String> {
    let mut table = root;
    for key in path {
        let at = match table.iter().position(|(k, _)| k == key) {
            Some(at) => at,
            None => {
                table.push((key.clone(), Value::Table(Vec::new())));
                table.len() - 1
            }
        };
        table = match &mut table[at].1 {
            Value::Table(t) => t,
            _ => {
                return Err(format!(
                    "`{key}` ya tiene un valor y no puede ser una tabla"
                ));
            }
        };
    }
    Ok(table)
}

fn insert(root: &mut Table, path: &[String], value: Value) -> Result<(), String> {
    let (last, parents) = path.split_last().expect("keys are never empty");
    let table = table_at(root, parents)?;
    if table.iter().any(|(k, _)| k == last) {
        return Err(format!("`{}` aparece dos veces", path.join(".")));
    }
    table.push((last.clone(), value));
    Ok(())
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    line: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.i + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn error(&self, message: &str) -> ParseError {
        ParseError {
            line: self.line,
            message: message.to_owned(),
        }
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.bump();
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) {
                self.bump();
            }
        }
    }

    /// Spaces, comments and newlines (between statements and inside arrays).
    fn skip_blank_lines(&mut self) {
        loop {
            self.skip_spaces();
            self.skip_comment();
            match self.peek() {
                Some('\n') | Some('\r') => {
                    self.bump();
                }
                _ => return,
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), ParseError> {
        self.skip_spaces();
        self.skip_comment();
        if self.peek() == Some('\r') {
            self.bump();
        }
        match self.peek() {
            None => Ok(()),
            Some('\n') => {
                self.bump();
                Ok(())
            }
            Some(c) => Err(self.error(&format!(
                "sobra `{c}` al final de la línea (¿falta una coma o unas comillas?)"
            ))),
        }
    }

    fn key_path(&mut self) -> Result<Vec<String>, ParseError> {
        let mut path = vec![self.key()?];
        loop {
            self.skip_spaces();
            if self.peek() != Some('.') {
                return Ok(path);
            }
            self.bump();
            self.skip_spaces();
            path.push(self.key()?);
        }
    }

    fn key(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Some('"') => self.basic_string(),
            Some('\'') => self.literal_string(),
            _ => {
                let start = self.i;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    self.bump();
                }
                if self.i == start {
                    let message = match self.peek() {
                        Some(c) => format!(
                            "esperaba un nombre (letras, números, `_` o `-`) y encontré `{c}`"
                        ),
                        None => "esperaba un nombre y el archivo terminó".to_owned(),
                    };
                    return Err(self.error(&message));
                }
                Ok(self.chars[start..self.i].iter().collect())
            }
        }
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        match self.peek() {
            Some('"') => self.basic_string().map(Value::Str),
            Some('\'') => self.literal_string().map(Value::Str),
            Some('[') => self.array(),
            Some('{') => self.inline_table(),
            Some('t') | Some('f') => {
                let word = self.word();
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    _ => Err(self.error(&format!(
                        "`{word}` no es un valor: los textos van entre comillas"
                    ))),
                }
            }
            Some(c) if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' => self.number(),
            Some('\n') | None => Err(self.error("falta el valor después del `=`")),
            Some(_) => {
                let word = self.word();
                Err(self.error(&format!(
                    "`{word}` no es un valor: los textos van entre comillas"
                )))
            }
        }
    }

    fn word(&mut self) -> String {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| !c.is_whitespace() && !matches!(c, ',' | ']' | '}' | '#'))
        {
            self.bump();
        }
        self.chars[start..self.i].iter().collect()
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let raw = self.word();
        let clean: String = raw.chars().filter(|c| *c != '_').collect();
        if let Ok(i) = clean.parse::<i64>() {
            return Ok(Value::Int(i));
        }
        let looks_numeric = clean
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
        match clean.parse::<f64>() {
            Ok(f) if looks_numeric && f.is_finite() => Ok(Value::Float(f)),
            _ => Err(self.error(&format!(
                "`{raw}` no es un número (las fechas no se admiten; los textos van entre comillas)"
            ))),
        }
    }

    fn triple(&self, quote: char) -> bool {
        self.peek() == Some(quote)
            && self.peek_at(1) == Some(quote)
            && self.peek_at(2) == Some(quote)
    }

    fn basic_string(&mut self) -> Result<String, ParseError> {
        let multi = self.triple('"');
        let start_line = self.line;
        self.bump();
        if multi {
            self.bump();
            self.bump();
            if self.peek() == Some('\n') {
                self.bump();
            }
        }
        let mut out = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(ParseError {
                        line: start_line,
                        message: "un texto empieza con `\"` y nunca se cierra".into(),
                    });
                }
                Some('\n') if !multi => {
                    return Err(self.error(
                        "un texto entre `\"` no puede seguir en otra línea (usá `\"\"\"`)",
                    ));
                }
                Some('"') if !multi => {
                    self.bump();
                    return Ok(out);
                }
                Some('"') if self.triple('"') => {
                    self.bump();
                    self.bump();
                    self.bump();
                    return Ok(out);
                }
                Some('\\') => {
                    self.bump();
                    let escaped = match self.bump() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('"') => '"',
                        Some('\\') => '\\',
                        Some('u') => {
                            let hex: String = (0..4).filter_map(|_| self.bump()).collect();
                            u32::from_str_radix(&hex, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .ok_or_else(|| {
                                    self.error(&format!("`\\u{hex}` no es una letra válida"))
                                })?
                        }
                        Some('\n') if multi => {
                            // A line-ending backslash joins the lines.
                            while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                                self.bump();
                            }
                            continue;
                        }
                        other => {
                            return Err(self.error(&format!(
                                "`\\{}` no se entiende dentro de un texto (para una barra escribí `\\\\`)",
                                other.unwrap_or(' ')
                            )));
                        }
                    };
                    out.push(escaped);
                }
                Some(c) => {
                    self.bump();
                    out.push(c);
                }
            }
        }
    }

    fn literal_string(&mut self) -> Result<String, ParseError> {
        let multi = self.triple('\'');
        let start_line = self.line;
        self.bump();
        if multi {
            self.bump();
            self.bump();
            if self.peek() == Some('\n') {
                self.bump();
            }
        }
        let mut out = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(ParseError {
                        line: start_line,
                        message: "un texto empieza con `'` y nunca se cierra".into(),
                    });
                }
                Some('\n') if !multi => {
                    return Err(self.error("un texto entre `'` no puede seguir en otra línea"));
                }
                Some('\'') if !multi => {
                    self.bump();
                    return Ok(out);
                }
                Some('\'') if self.triple('\'') => {
                    self.bump();
                    self.bump();
                    self.bump();
                    return Ok(out);
                }
                Some(c) => {
                    self.bump();
                    out.push(c);
                }
            }
        }
    }

    fn array(&mut self) -> Result<Value, ParseError> {
        let start_line = self.line;
        self.bump();
        let mut items = Vec::new();
        loop {
            self.skip_blank_lines();
            match self.peek() {
                None => {
                    return Err(ParseError {
                        line: start_line,
                        message: "una lista empieza con `[` y nunca se cierra".into(),
                    });
                }
                Some(']') => {
                    self.bump();
                    return Ok(Value::Array(items));
                }
                _ => {}
            }
            items.push(self.value()?);
            self.skip_blank_lines();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some(']') => {}
                Some(c) => {
                    return Err(self.error(&format!(
                        "en una lista los valores van separados por comas, y encontré `{c}`"
                    )));
                }
                None => {}
            }
        }
    }

    fn inline_table(&mut self) -> Result<Value, ParseError> {
        self.bump();
        let mut table: Table = Vec::new();
        self.skip_spaces();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(Value::Table(table));
        }
        loop {
            self.skip_spaces();
            let path = self.key_path()?;
            self.skip_spaces();
            if self.peek() != Some('=') {
                return Err(self.error("dentro de `{ }` cada valor va como `nombre = valor`"));
            }
            self.bump();
            self.skip_spaces();
            let value = self.value()?;
            insert(&mut table, &path, value).map_err(|m| self.error(&m))?;
            self.skip_spaces();
            match self.bump() {
                Some(',') => continue,
                Some('}') => return Ok(Value::Table(table)),
                _ => {
                    return Err(self.error(
                        "una tabla `{ ... }` tiene que cerrarse en la misma línea con `}`",
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_what_a_manifest_uses() {
        let text = r#"
# comentario
id = "kitty"          # al final también
version = '1.0'
escala = 0.5
fps = 8
activa = true

[cuadro]
ancla = [0.5, 0.0]

[animaciones.idle]
cuadros = [
    "a", "b",   # dos
]
"idle.raro" = 1
voz = { id = "ef_dora", velocidad = 1.1 }
alma = """
Primera línea.
Segunda."""
"#;
        let t = parse(text).unwrap();
        assert_eq!(get(&t, "id"), Some(&Value::Str("kitty".into())));
        assert_eq!(get(&t, "fps").and_then(Value::as_f64), Some(8.0));
        assert_eq!(get(&t, "activa"), Some(&Value::Bool(true)));
        let anims = get(&t, "animaciones").and_then(Value::as_table).unwrap();
        let idle = get(anims, "idle").and_then(Value::as_table).unwrap();
        assert_eq!(
            get(idle, "cuadros")
                .and_then(Value::as_array)
                .map(|a| a.len()),
            Some(2)
        );
        assert_eq!(get(idle, "idle.raro"), Some(&Value::Int(1)));
        let voz = get(idle, "voz").and_then(Value::as_table).unwrap();
        assert_eq!(get(voz, "velocidad").and_then(Value::as_f64), Some(1.1));
        assert_eq!(
            get(idle, "alma").and_then(Value::as_str),
            Some("Primera línea.\nSegunda.")
        );
        let cuadro = get(&t, "cuadro").and_then(Value::as_table).unwrap();
        assert_eq!(
            get(cuadro, "ancla"),
            Some(&Value::Array(vec![Value::Float(0.5), Value::Float(0.0)]))
        );
    }

    #[test]
    fn mistakes_say_where_and_what_in_spanish() {
        let e = parse("id = kitty\n").unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.message.contains("comillas"), "{e}");
        let e = parse("a = 1\n\nb = \"sin cerrar\n").unwrap_err();
        assert_eq!(e.line, 3);
        let e = parse("a = 1\na = 2\n").unwrap_err();
        assert!(e.message.contains("dos veces"), "{e}");
        let e = parse("[[x]]\n").unwrap_err();
        assert!(e.message.contains("[[...]]"), "{e}");
        let e = parse("a = [1, 2\n").unwrap_err();
        assert!(e.message.contains("nunca se cierra"), "{e}");
        let e = parse("fecha = 2024-01-01\n").unwrap_err();
        assert!(e.message.contains("fechas"), "{e}");
        let e = parse("a = 1 2\n").unwrap_err();
        assert!(e.message.contains("sobra"), "{e}");
    }

    #[test]
    fn tables_and_keys_do_not_clash_silently() {
        assert!(parse("[a]\n[a]\n").is_err());
        assert!(parse("a = 1\n[a]\n").is_err());
        assert!(parse("[a.b]\nx = 1\n[a]\ny = 2\n").is_ok());
    }
}
