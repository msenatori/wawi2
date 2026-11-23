//! Choosing what to work on (Wawi 1's rules, smaller):
//!
//! - **What qualifies.** A report from the owner, the brain going down, or
//!   a problem from the nightly review, always. Anything else once it has
//!   repeated (`min_repeticiones`). Never one the owner discarded, one with
//!   a proposal waiting, or one that failed less than `reintento_dias` ago.
//!   An applied fix counts as back only after it repeats again.
//! - **What comes first** is what costs the owner most: she was gone, he
//!   told her, she was slow (speed is his hard rule), she did not
//!   understand, then the rest. More occurrences rank higher.

use crate::config::Config;
use crate::diario::{Estado, Problema};

/// Why a problem may not be attempted now, or `Ok`.
pub fn elegible(p: &Problema, config: &Config, now: i64) -> Result<(), &'static str> {
    match p.estado {
        Estado::Descartado => return Err("descartado"),
        Estado::Propuesto => return Err("propuesto"),
        Estado::Aplicado => {
            if p.reportado() || p.cuenta < p.cuenta_al_aplicar + config.min_repeticiones {
                return Err("ya_arreglado");
            }
        }
        Estado::Abierto => {}
    }
    if let Some(failed) = p.ultimo_fallo()
        && now - failed < i64::from(config.reintento_dias) * 86_400
    {
        return Err("espera");
    }
    let always = matches!(p.tipo.as_str(), "reportado" | "cerebro_caido" | "revision");
    if always || p.cuenta >= config.min_repeticiones {
        Ok(())
    } else {
        Err("poco_frecuente")
    }
}

/// How much fixing it helps the owner. Higher first.
pub fn beneficio(p: &Problema) -> f64 {
    let base = match p.tipo.as_str() {
        "cerebro_caido" => 130.0,
        "reportado" => 100.0,
        "turno_lento" => 95.0,
        "transcripcion_vacia" => 80.0,
        "audio" => 75.0,
        "turno_fallido" => 72.0,
        "plugin_caido" => 60.0,
        "revision" => 50.0,
        _ => 40.0,
    };
    base + 6.0 * f64::from(p.cuenta).ln_1p()
}

/// The eligible problems, most useful first.
pub fn ordenar<'a>(problemas: &'a [Problema], config: &Config, now: i64) -> Vec<&'a Problema> {
    let mut eligible: Vec<&Problema> = problemas
        .iter()
        .filter(|p| elegible(p, config, now).is_ok())
        .collect();
    eligible.sort_by(|a, b| {
        beneficio(b)
            .total_cmp(&beneficio(a))
            .then(b.cuenta.cmp(&a.cuenta))
            .then(a.id.cmp(&b.id))
    });
    eligible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diario::Intento;

    fn p(tipo: &str, cuenta: u32) -> Problema {
        Problema {
            id: format!("{tipo}-{cuenta}"),
            tipo: tipo.into(),
            clave: String::new(),
            titulo: String::new(),
            cuenta,
            primero_ts: 0,
            ultimo_ts: 0,
            muestras: Vec::new(),
            estado: Estado::Abierto,
            intentos: Vec::new(),
            propuesta: None,
            cuenta_al_aplicar: 0,
        }
    }

    #[test]
    fn crashes_and_reports_first_and_rare_ones_wait() {
        let c = Config::default();
        let list = vec![
            p("plugin_caido", 9),
            p("turno_lento", 3),
            p("reportado", 1),
            p("audio", 1),
            p("cerebro_caido", 1),
        ];
        let order: Vec<&str> = ordenar(&list, &c, 0)
            .iter()
            .map(|p| p.tipo.as_str())
            .collect();
        assert_eq!(
            order,
            ["cerebro_caido", "reportado", "turno_lento", "plugin_caido"]
        );
    }

    #[test]
    fn a_failure_waits_a_week_and_a_discard_is_forever() {
        let c = Config::default();
        let mut x = p("reportado", 1);
        x.intentos.push(Intento {
            id: "a".into(),
            ts: 100,
            resultado: "rechazado".into(),
            motivo: "tests".into(),
            costo_usd: 1.0,
        });
        assert_eq!(elegible(&x, &c, 100 + 86_400), Err("espera"));
        assert!(elegible(&x, &c, 100 + 8 * 86_400).is_ok());
        x.estado = Estado::Descartado;
        assert_eq!(elegible(&x, &c, 100 + 80 * 86_400), Err("descartado"));
        let mut y = p("turno_lento", 5);
        y.estado = Estado::Aplicado;
        y.cuenta_al_aplicar = 5;
        assert_eq!(elegible(&y, &c, 0), Err("ya_arreglado"));
        y.cuenta = 7;
        assert!(elegible(&y, &c, 0).is_ok(), "it came back");
    }
}
