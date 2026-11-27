//! The page's size and guides.

use super::*;

pub(in crate::cmd::layout) fn page(s: &mut Session, p: &Value) -> CmdResult {
    let i = index(s, p)?;
    let cur = s.project.layouts.get(i).map(|l| (l.width, l.height));
    let (w, h) = page_size(p, cur)?;
    let comma = decimal_mark(p)?;
    // The page's colour (#2855): absent keeps it, null or white gives the white page back.
    let paper = crate::cmd::opaque_color(p, "color")?;
    let l = s.project.layouts.get_mut(i).ok_or("no layout")?;
    l.width = w;
    l.height = h;
    if let Some(comma) = comma {
        l.decimal_comma = comma;
    }
    if let Some(paper) = paper {
        l.paper = paper.filter(|c| *c != Rgba::rgb(0xFF, 0xFF, 0xFF));
    }
    let color = l.paper.map(Rgba::hex);
    Ok(json!({"width": w, "height": h, "decimal": if l.decimal_comma { "," } else { "." }, "color": color}))
}

/// Sets a layout's ruler guides: the lists given replace those there.
pub(in crate::cmd::layout) fn guides(s: &mut Session, p: &Value) -> CmdResult {
    let read = |k: &str| -> Result<Option<Vec<f64>>, String> {
        let bad = || format!("{k} is a list of positions in mm");
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(a)) if a.len() <= MAX_GUIDES => a
                .iter()
                .map(|v| v.as_f64().filter(|v| v.is_finite() && v.abs() <= 5000.0).ok_or_else(bad))
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(Value::Array(_)) => Err(format!("a layout has at most {MAX_GUIDES} guides each way")),
            Some(_) => Err(bad()),
        }
    };
    // Checked before anything changes.
    let (x, y) = (read("x")?, read("y")?);
    let l = active(s, p)?;
    if b(p, "clear") == Some(true) {
        l.guides = Guides::default();
    }
    if let Some(x) = x {
        l.guides.x = x;
    }
    if let Some(y) = y {
        l.guides.y = y;
    }
    Ok(json!({"x": l.guides.x, "y": l.guides.y}))
}
