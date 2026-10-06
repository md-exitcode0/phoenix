//! Semantic palette generation adapted from TasteCode's palette.ts.
//! Copyright 2026 TasteCode contributors. Apache-2.0; see licenses/tastecode/.
//! Phoenix changes: native Rust implementation, bounded typed input, no I/O.

use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const ROLES: [&str; 12] = ["canvas", "surface", "surfaceAlt", "text", "textMuted", "divider", "controlBorder", "accent", "accentHover", "onAccent", "accentText", "focusRing"];
const REPAIR_ORDER: [usize; 11] = [9, 10, 4, 3, 6, 11, 8, 7, 2, 1, 0];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Direction {
    accent_seed: String,
    #[serde(default)]
    neutral_seed: Option<String>,
    surface_contrast: SurfaceContrast,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SurfaceContrast { Quiet, Defined }

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    themes: BTreeMap<String, Direction>,
    #[serde(default)]
    locked: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Color([u8; 3]);

impl Color {
    fn parse(s: &str) -> Result<Self> {
        if !s.starts_with('#') || !matches!(s.len(), 4 | 7) || !s[1..].bytes().all(|c| c.is_ascii_hexdigit()) {
            bail!("Colors must be opaque sRGB #RGB or #RRGGBB values: {s:?}");
        }
        let h = if s.len() == 4 { s[1..].chars().flat_map(|c| [c, c]).collect::<String>() } else { s[1..].into() };
        Ok(Self([u8::from_str_radix(&h[0..2], 16)?, u8::from_str_radix(&h[2..4], 16)?, u8::from_str_radix(&h[4..6], 16)?]))
    }
    fn hex(self) -> String { format!("#{:02X}{:02X}{:02X}", self.0[0], self.0[1], self.0[2]) }
    fn linear(self) -> [f64; 3] { self.0.map(|c| { let n = c as f64 / 255.; if n <= 0.04045 { n / 12.92 } else { ((n + 0.055) / 1.055).powf(2.4) } }) }
    fn luminance(self) -> f64 { let [r, g, b] = self.linear(); 0.2126 * r + 0.7152 * g + 0.0722 * b }
    fn lch(self) -> [f64; 3] {
        let [r, g, b] = self.linear();
        let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
        let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
        let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
        let a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
        let b = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
        let c = a.hypot(b);
        [0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s, c, if c < 1e-7 { 0. } else { b.atan2(a).to_degrees().rem_euclid(360.) }]
    }
    fn from_lch([l, c, h]: [f64; 3]) -> Self {
        fn linear(l: f64, c: f64, h: f64) -> [f64; 3] {
            let a = c * h.to_radians().cos(); let b = c * h.to_radians().sin();
            let lc = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
            let mc = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
            let sc = (l - 0.0894841775 * a - 1.291485548 * b).powi(3);
            [4.0767416621 * lc - 3.3077115913 * mc + 0.2309699292 * sc, -1.2684380046 * lc + 2.6097574011 * mc - 0.3413193965 * sc, -0.0041960863 * lc - 0.7034186147 * mc + 1.707614701 * sc]
        }
        let (mut low, mut high) = (0., c);
        let mut best = linear(l, 0., h);
        for _ in 0..24 {
            let chroma = (low + high) / 2.; let next = linear(l, chroma, h);
            if next.iter().all(|v| (0.0..=1.0).contains(v)) { best = next; low = chroma; } else { high = chroma; }
        }
        Self(best.map(|n| { let n = n.clamp(0., 1.); let c = if n <= 0.0031308 { n * 12.92 } else { 1.055 * n.powf(1. / 2.4) - 0.055 }; (c * 255.).round() as u8 }))
    }
}

fn contrast(a: Color, b: Color) -> f64 { let x = a.luminance(); let y = b.luminance(); (x.max(y) + 0.05) / (x.min(y) + 0.05) }

fn pairs() -> Vec<(usize, usize, f64)> {
    let mut p = Vec::with_capacity(17);
    for fg in [3, 4, 10] { for bg in 0..3 { p.push((fg, bg, 4.5)); } }
    p.extend([(9, 7, 4.5), (9, 8, 4.5)]);
    for fg in [6, 11] { for bg in 0..3 { p.push((fg, bg, 3.)); } }
    p
}

fn checks(roles: &[Color; 12]) -> Vec<Value> {
    pairs().iter().map(|&(f,b,min)| { let ratio = contrast(roles[f], roles[b]); json!({"foreground":ROLES[f],"background":ROLES[b],"ratio":ratio,"minimum":min,"criterion":if min==4.5 {"WCAG 2.2 1.4.3"} else {"WCAG 2.2 1.4.11"},"pass":ratio>=min}) }).collect()
}
fn score(roles: &[Color; 12]) -> (usize, f64) {
    pairs().iter().fold((0,0.), |(count,gap), &(f,b,min)| { let c=contrast(roles[f],roles[b]); if c<min {(count+1,gap+min-c)}else{(count,gap)} })
}

fn repair(roles: &[Color;12], locked: &[bool;12]) -> Option<(usize, Color)> {
    let current=score(roles); let ps=pairs();
    let mut best: Option<(usize,Color,usize,f64,f64,usize)>=None;
    for (order,&role) in REPAIR_ORDER.iter().enumerate() {
        if locked[role] || !ps.iter().any(|&(f,b,min)| (role==f||role==b)&&contrast(roles[f],roles[b])<min) {continue;}
        let [old_l,c,h]=roles[role].lch();
        for step in 0..=500 {
            let l=step as f64/500.; let color=Color::from_lch([l,c,h]); if color==roles[role] {continue;}
            let mut next=*roles; next[role]=color; let (fails,gap)=score(&next);
            if fails>current.0 || (fails==current.0&&gap>=current.1-1e-9) {continue;}
            let dist=(l-old_l).abs();
            let better=best.as_ref().is_none_or(|&(_,bc,bf,bg,bd,bo)| (fails, gap, dist, order, color.0)<(bf,bg,bd,bo,bc.0));
            if better {best=Some((role,color,fails,gap,dist,order));}
        }
    }
    best.map(|(role,color,_,_,_,_)|(role,color))
}

pub fn generate(request: Request) -> Result<Value> {
    if request.themes.is_empty() || request.themes.len()>2 {bail!("Provide one or both of light/dark themes");}
    for theme in request.themes.keys().chain(request.locked.keys()) {
        if !matches!(theme.as_str(),"light"|"dark") || !request.themes.contains_key(theme) {bail!("Unknown or missing theme direction: {theme}");}
    }
    let mut themes=serde_json::Map::new(); let mut issues=Vec::new();
    for (name,direction) in request.themes {
        let accent_seed=Color::parse(&direction.accent_seed)?;
        let mut locks=[None;12];
        if let Some(values)=request.locked.get(&name) {
            for (role,value) in values {
                let Some(i)=ROLES.iter().position(|r|r==role)else{bail!("Unknown palette role: {role}");};
                locks[i]=Some(Color::parse(value)?);
            }
        }
        let accent=locks[7].unwrap_or(accent_seed);
        let [al,ac,ah]=accent.lch();
        let [_,nc,nh]=direction.neutral_seed.as_deref().map(Color::parse).transpose()?.unwrap_or(accent_seed).lch();
        let hue=if nc>0.001 {nh}else{ah};
        let n=|l,c:f64| Color::from_lch([l,nc.min(c),hue]);
        let a=|l,c|Color::from_lch([l,c,ah]);
        let dark=name=="dark"; let defined=matches!(direction.surface_contrast,SurfaceContrast::Defined);
        let white=Color([255;3]); let black=Color([0;3]);
        let mut roles=[
            n(if dark{0.12}else{0.975},0.025), n(if dark{0.17}else{0.995},0.018),
            n(if dark{if defined{0.28}else{0.22}}else if defined{0.9}else{0.94},0.025),
            n(if dark{0.94}else{0.18},0.015),n(if dark{0.7}else{0.42},0.015),
            n(if dark{if defined{0.38}else{0.3}}else if defined{0.82}else{0.88},0.02),
            n(if dark{0.62}else{0.52},0.025),accent,
            a((al+if dark{0.07}else{-0.07}).clamp(0.03,0.97),ac),
            if contrast(white,accent)>=4.5{white}else{black},
            a(if dark{0.72}else{0.38},ac.min(0.16)),a(if dark{0.7}else{0.48},ac.min(0.18))
        ];
        for (i,c) in locks.iter().enumerate(){if let Some(c)=c{roles[i]=*c;}}
        let mut immutable=locks.map(|v|v.is_some()); immutable[7]=true;
        let conflicts=pairs().into_iter().filter(|&(f,b,min)|immutable[f]&&immutable[b]&&contrast(roles[f],roles[b])<min).collect::<Vec<_>>();
        let mut repairs=Vec::new();
        if conflicts.is_empty() {
            for _ in 0..24 {
                if score(&roles).0==0{break;}
                let Some((i,color))=repair(&roles,&immutable)else{break;};
                let affected=pairs().into_iter().filter(|&(f,b,min)|(i==f||i==b)&&contrast(roles[f],roles[b])<min).map(|(f,b,_)|format!("{}/{}",ROLES[f],ROLES[b])).collect::<Vec<_>>();
                repairs.push(json!({"role":ROLES[i],"from":roles[i].hex(),"to":color.hex(),"reason":"contrast","affectedPairs":affected})); roles[i]=color;
            }
        }
        let checked=checks(&roles);
        for pair in checked.iter().filter(|p|p["pass"]==false) {
            issues.push(json!({"code":if conflicts.is_empty(){"unrepairable-contrast"}else{"locked-contrast-conflict"},"theme":name,"foreground":pair["foreground"],"background":pair["background"],"ratio":pair["ratio"],"minimum":pair["minimum"]}));
        }
        let values=ROLES.iter().enumerate().map(|(i,key)|(key.to_string(),json!(roles[i].hex()))).collect::<serde_json::Map<_,_>>();
        let css=ROLES.iter().enumerate().map(|(i,key)| {let slug=key.chars().flat_map(|c|if c.is_uppercase(){vec!['-',c.to_ascii_lowercase()]}else{vec![c]}).collect::<String>();format!("  --color-{slug}: {};",roles[i].hex())}).collect::<Vec<_>>().join("\n");
        themes.insert(name,json!({"roles":values,"lockedRoles":ROLES.iter().enumerate().filter(|(i,_)|locks[*i].is_some()).map(|(_,k)|*k).collect::<Vec<_>>(),"repairs":repairs,"checks":checked,"css":css}));
    }
    Ok(json!({"status":if issues.is_empty(){"ready"}else{"blocked"},"themes":themes,"issues":issues,"evidenceScope":"opaque-srgb-token-pairs","limitation":"Token contrast is not rendered accessibility or visual-quality acceptance. Preserve locked brand colors; change their use or pairings when blocked."}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(v:Value)->Result<Value>{generate(serde_json::from_value(v)?)}
    #[test] fn both_themes_have_readable_tokens_and_exact_accent(){
        let out=run(json!({"themes":{"light":{"accentSeed":"#C1492E","surfaceContrast":"quiet"},"dark":{"accentSeed":"#E5A28B","surfaceContrast":"defined"}}})).unwrap();
        assert_eq!(out["status"],"ready");
        for theme in ["light","dark"]{assert_eq!(out["themes"][theme]["roles"].as_object().unwrap().len(),12);assert_eq!(out["themes"][theme]["checks"].as_array().unwrap().len(),17);assert!(out["themes"][theme]["checks"].as_array().unwrap().iter().all(|r|r["pass"]==true));}
        assert_eq!(out["themes"]["light"]["roles"]["accent"],"#C1492E");
        assert_ne!(out["themes"]["light"]["roles"]["canvas"],out["themes"]["dark"]["roles"]["canvas"]);
    }
    #[test] fn locked_failure_does_not_recolor_brand_or_claim_pass(){
        let out=run(json!({"themes":{"light":{"accentSeed":"#fff","surfaceContrast":"quiet"}},"locked":{"light":{"accent":"#ffffff","onAccent":"#fff"}}})).unwrap();
        assert_eq!(out["status"],"blocked");assert_eq!(out["themes"]["light"]["roles"]["onAccent"],"#FFFFFF");assert!(!out["issues"].as_array().unwrap().is_empty());
    }
    #[test] fn malformed_theme_color_and_role_are_rejected(){
        for v in [json!({"themes":{}}),json!({"themes":{"neon":{}}}),json!({"themes":{"light":{"accentSeed":"#fff0","surfaceContrast":"quiet"}}}),json!({"themes":{"light":{"accentSeed":"#fff","surfaceContrast":"quiet"}},"locked":{"light":{"body":"#000"}}})]{assert!(run(v).is_err());}
        for s in ["red","#12","#👾","#zzzzzz","rgb(0 0 0)"]{assert!(Color::parse(s).is_err());}
    }
    #[test] fn contrast_matches_black_white_control(){assert!((contrast(Color([0;3]),Color([255;3]))-21.).abs()<1e-9);}
}
