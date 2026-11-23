//! `recordar` and `olvidar`: the owner's long-term facts in MEMORY.md. A
//! change reaches the brain on the next turn through the memory block the host
//! already sends; nothing here talks to the brain.

use std::rc::Rc;

use serde_json::json;
use wawi_core::{EventSink, PermissionLevel, normalize};
use wawi_memory::{Home, MemoryError};

use crate::tool::{InstantTool, known_args, spec, text_arg};

pub const REMEMBER: &str = "recordar";
pub const FORGET: &str = "olvidar";

const REMEMBER_DESCRIPTION: &str = "Guarda en tu memoria de largo plazo un dato del dueño \
o de su vida (gustos, nombres, datos de la casa, decisiones) para tenerlo en las próximas \
charlas. Usala cuando el dueño te pida que te acuerdes de algo o te cuente algo que \
claramente le va a importar después. Un dato por llamada, en una frase corta en tercera \
persona (por ejemplo «Toma el café sin azúcar»). No es para avisos con hora: para eso está \
avisame. Nunca guardes contraseñas, claves ni tokens.";

const FORGET_DESCRIPTION: &str = "Borra un dato de tu memoria de largo plazo. Pasá el \
texto tal como aparece en tu memoria (sin la fecha). Si no hay uno igual, no borra nada y \
te devuelve los más parecidos: si alguno es el que el dueño quiso decir, volvé a llamarla \
con ese texto exacto.";

/// How many near matches `olvidar` offers when there is no exact one.
const CANDIDATES: usize = 3;

/// Saves a fact dated `date` (`YYYY-MM-DD`, the local day). The reply is
/// for the brain, or for the simulator's `/remember`.
pub fn remember(home: &Home, fact: &str, date: &str) -> Result<String, String> {
    match home.add_fact(fact, date) {
        Ok(true) => Ok(format!("Guardado: «{}».", fact.trim())),
        Ok(false) => Ok(format!("Ya lo tenía guardado: «{}».", fact.trim())),
        Err(MemoryError::Secret) => {
            Err("No lo guardé: parece una contraseña, clave o token, y eso no se guarda.".into())
        }
        Err(MemoryError::MultiLine) => Err("Un dato tiene que ser una sola línea.".into()),
        Err(MemoryError::Empty) => Err("El dato está vacío.".into()),
        Err(error) => Err(format!("No pude guardarlo: {error}")),
    }
}

/// Deletes the fact that matches `fact` exactly, ignoring case, accents and
/// punctuation. Without an exact match nothing is deleted and the reply
/// lists the closest facts instead.
pub fn forget(home: &Home, fact: &str) -> Result<String, String> {
    let wanted = normalize(fact);
    if wanted.is_empty() {
        return Err("El dato está vacío.".into());
    }
    let memory = home
        .memory()
        .map_err(|error| format!("No pude leer la memoria: {error}"))?;
    let exact: Vec<&str> = memory
        .facts
        .iter()
        .filter(|known| normalize(&known.text) == wanted)
        .map(|known| known.text.as_str())
        .collect();
    if let Some(&text) = exact.first() {
        let mut texts = exact.clone();
        texts.dedup();
        for text in texts {
            home.forget_fact(text)
                .map_err(|error| format!("No pude borrarlo: {error}"))?;
        }
        return Ok(format!("Olvidado: «{text}»."));
    }
    let close = closest(memory.facts.iter().map(|known| known.text.as_str()), fact);
    if close.is_empty() {
        return Err(format!(
            "No borré nada: no tengo ningún dato parecido a «{}».",
            fact.trim()
        ));
    }
    let list: Vec<String> = close.iter().map(|text| format!("«{text}»")).collect();
    Err(format!(
        "No borré nada: no hay un dato exactamente así. Los más parecidos: {}. \
         Si es uno de esos, llamá {FORGET} otra vez con el texto exacto.",
        list.join("; ")
    ))
}

/// The facts most like `query`, best first: the share of the query's words
/// (three letters or more) that the fact has, plus a bonus when one text
/// contains the other. Only facts with something in common come back.
pub fn closest<'a>(facts: impl Iterator<Item = &'a str>, query: &str) -> Vec<&'a str> {
    let query_norm = normalize(query);
    let words: Vec<&str> = query_norm
        .split(' ')
        .filter(|word| word.chars().count() >= 3)
        .collect();
    let mut scored: Vec<(f64, usize, &str)> = facts
        .enumerate()
        .filter_map(|(index, fact)| {
            let fact_norm = normalize(fact);
            let fact_words: Vec<&str> = fact_norm.split(' ').collect();
            let shared = words.iter().filter(|w| fact_words.contains(w)).count();
            let mut score = if words.is_empty() {
                0.0
            } else {
                shared as f64 / words.len() as f64
            };
            if !query_norm.is_empty()
                && (fact_norm.contains(&query_norm) || query_norm.contains(&fact_norm))
            {
                score += 1.0;
            }
            (score >= 0.34).then_some((score, index, fact))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(CANDIDATES)
        .map(|(_, _, fact)| fact)
        .collect()
}

/// `recordar(dato)`. `today` gives the local date for the fact.
pub fn remember_tool(
    home: Home,
    today: Box<dyn Fn() -> String>,
    sink: Rc<dyn EventSink>,
) -> InstantTool {
    let schema = json!({
        "type": "object",
        "properties": {
            "dato": { "type": "string", "description": "El dato, en una frase corta." }
        },
        "required": ["dato"],
        "additionalProperties": false
    });
    InstantTool::new(
        spec(
            REMEMBER,
            REMEMBER_DESCRIPTION,
            schema,
            PermissionLevel::Write,
        ),
        Box::new(move |args| {
            known_args(args, &["dato"])?;
            remember(&home, text_arg(args, "dato")?, &today())
        }),
        sink,
    )
}

/// `olvidar(dato)`.
pub fn forget_tool(home: Home, sink: Rc<dyn EventSink>) -> InstantTool {
    let schema = json!({
        "type": "object",
        "properties": {
            "dato": {
                "type": "string",
                "description": "El texto del dato tal como está en tu memoria, sin la fecha."
            }
        },
        "required": ["dato"],
        "additionalProperties": false
    });
    InstantTool::new(
        spec(FORGET, FORGET_DESCRIPTION, schema, PermissionLevel::Write),
        Box::new(move |args| {
            known_args(args, &["dato"])?;
            forget(&home, text_arg(args, "dato")?)
        }),
        sink,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use wawi_core::{Event, Tool, ToolCall, ToolOutcome, TurnId};

    fn home(name: &str) -> Home {
        let dir =
            std::env::temp_dir().join(format!("wawi2-tools-memory-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Home::open(dir).unwrap()
    }

    fn texts(home: &Home) -> Vec<String> {
        home.memory()
            .unwrap()
            .facts
            .into_iter()
            .map(|fact| format!("{}: {}", fact.date, fact.text))
            .collect()
    }

    #[derive(Default)]
    struct Collect(RefCell<Vec<Event>>);

    impl EventSink for Collect {
        fn send(&self, event: Event) {
            self.0.borrow_mut().push(event);
        }
    }

    fn call(tool: &mut InstantTool, sink: &Collect, args: &str) -> ToolOutcome {
        let name = tool.spec().name.clone();
        tool.start(
            TurnId(1),
            &ToolCall {
                id: "c1".into(),
                name,
                args_json: args.into(),
            },
        );
        match sink.0.borrow_mut().pop() {
            Some(Event::ToolFinished {
                outcome, call_id, ..
            }) => {
                assert_eq!(call_id, "c1");
                outcome
            }
            other => panic!("expected ToolFinished, got {other:?}"),
        }
    }

    #[test]
    fn remember_saves_with_the_local_date_and_no_confirmation() {
        let home = home("remember");
        let sink = Rc::new(Collect::default());
        let mut tool = remember_tool(home.clone(), Box::new(|| "2026-11-20".into()), sink.clone());
        assert_eq!(tool.spec().permission, PermissionLevel::Write);
        assert!(!tool.spec().needs_confirmation);
        assert_eq!(
            call(&mut tool, &sink, r#"{"dato":"Toma el café sin azúcar"}"#),
            ToolOutcome::Success("Guardado: «Toma el café sin azúcar».".into())
        );
        assert_eq!(texts(&home), ["2026-11-20: Toma el café sin azúcar"]);
        assert!(matches!(
            call(&mut tool, &sink, r#"{"dato":"Toma el café sin azúcar"}"#),
            ToolOutcome::Success(text) if text.starts_with("Ya lo tenía")
        ));
        assert!(home.context().unwrap().contains("Toma el café sin azúcar"));
    }

    #[test]
    fn remember_refuses_secrets_blanks_and_bad_arguments() {
        let home = home("refuse");
        let sink = Rc::new(Collect::default());
        let mut tool = remember_tool(home.clone(), Box::new(|| "2026-11-20".into()), sink.clone());
        for args in [
            r#"{"dato":"mi contraseña es hunter2"}"#,
            r#"{"dato":"  "}"#,
            r#"{"dato":"una\nlínea"}"#,
            r#"{"dato":3}"#,
            r#"{}"#,
            r#"{"dato":"x","fecha":"hoy"}"#,
            "[]",
        ] {
            assert!(
                matches!(call(&mut tool, &sink, args), ToolOutcome::Failure(_)),
                "{args}"
            );
        }
        assert!(texts(&home).is_empty());
    }

    #[test]
    fn forget_deletes_only_an_exact_match() {
        let home = home("forget");
        home.add_fact("Toma el café sin azúcar", "2026-11-01")
            .unwrap();
        home.add_fact("Su perra se llama Luna", "2026-11-02")
            .unwrap();
        home.add_fact("Toma mate a la tarde", "2026-11-03").unwrap();
        let sink = Rc::new(Collect::default());
        let mut tool = forget_tool(home.clone(), sink.clone());

        let outcome = call(&mut tool, &sink, r#"{"dato":"el café"}"#);
        let ToolOutcome::Failure(reply) = outcome else {
            panic!("a near match must not delete: {outcome:?}");
        };
        assert!(reply.contains("«Toma el café sin azúcar»"), "{reply}");
        assert!(!reply.contains("Luna"), "{reply}");
        assert_eq!(texts(&home).len(), 3, "nothing was deleted");

        // Case, accents and punctuation do not matter; the words do.
        assert_eq!(
            call(&mut tool, &sink, r#"{"dato":"toma el cafe sin azucar."}"#),
            ToolOutcome::Success("Olvidado: «Toma el café sin azúcar».".into())
        );
        assert_eq!(
            texts(&home),
            [
                "2026-11-03: Toma mate a la tarde",
                "2026-11-02: Su perra se llama Luna"
            ]
        );

        let outcome = call(&mut tool, &sink, r#"{"dato":"le gusta el jazz"}"#);
        assert!(
            matches!(&outcome, ToolOutcome::Failure(text) if text.contains("ningún dato parecido")),
            "{outcome:?}"
        );
    }

    #[test]
    fn closest_ranks_by_shared_words() {
        let facts = [
            "Su perra se llama Luna",
            "Toma mate a la tarde",
            "Toma el café sin azúcar",
        ];
        assert_eq!(
            closest(facts.iter().copied(), "toma café"),
            ["Toma el café sin azúcar", "Toma mate a la tarde"]
        );
        assert_eq!(
            closest(facts.iter().copied(), "luna"),
            ["Su perra se llama Luna"]
        );
        assert!(closest(facts.iter().copied(), "jazz").is_empty());
    }
}
