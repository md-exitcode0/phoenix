//! Phoenix's offline design utilities. No model, network, shell, or file writes.
//! Adapted from TasteCode design-agent: Copyright 2026 TasteCode contributors,
//! Apache-2.0. See licenses/tastecode/ and docs/iris-design.md for the port map.
//! Phoenix changes: native, bounded tools; honest availability copy is retained;
//! original-user naming and rendered comprehension outrank mechanical style rules.

mod palette;
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use super::ToolOutput;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesignStudioInput {
    Palette { request: palette::Request },
    Typography { seed: String },
    ReviewCopy { page: Page },
    ReviewBlueprint { page: Page },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    product: String,
    category: String,
    audience: String,
    offer: String,
    primary_action: String,
    sections: Vec<Section>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    id: String,
    heading: String,
    #[serde(default)] body: Vec<String>,
    #[serde(default)] actions: Vec<Action>,
    #[serde(default)] evidence: Vec<String>,
    #[serde(default)] user_question: String,
    #[serde(default)] purpose: String,
    #[serde(default)] dependencies: Vec<String>,
    #[serde(default)] composition: String,
    #[serde(default)] compact: String,
    #[serde(default)] motion: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Action { label: String, target: String }

fn bounded(text:&str, name:&str, max:usize, required:bool)->Result<()> {
    if text.len()>max || (required&&text.trim().is_empty()){bail!("{name} must {}fit within {max} bytes",if required{"be nonempty and "}else{""});}
    Ok(())
}

pub fn execute(input: DesignStudioInput)->Result<ToolOutput>{
    let (label,value)=match input {
        DesignStudioInput::Palette{request}=>("semantic palette",palette::generate(request)?),
        DesignStudioInput::Typography{seed}=>{
            bounded(&seed,"seed",1024,true)?;
            let mut pools=serde_json::from_str::<serde_json::Map<String,Value>>(include_str!("../../prompts/frontend/taste/typography-pool.json"))?;
            for (category,pool) in &mut pools {
                let Some(fonts)=pool.as_array_mut()else{bail!("Invalid embedded font pool");};
                fonts.sort_by_key(|name|Sha256::digest(format!("{seed}\0{category}\0{}",name.as_str().unwrap_or_default()).as_bytes()).to_vec());
            }
            ("typography candidates",json!({"seed":seed,"candidates":pools,"rule":"Preserve an explicit or existing brand face. For a new identity, match the reference's type category and try its first candidate. Save this seed with the design plan; do not reroll to recover a habitual face.","availability":"Names only. These fonts are not installed or licensed by this tool. Acquire the chosen face from its official source, retain its license and verify its loaded face and weight in the browser."}))
        },
        DesignStudioInput::ReviewCopy{page}=>("copy review",review(page,false)?),
        DesignStudioInput::ReviewBlueprint{page}=>("page blueprint review",review(page,true)?),
    };
    Ok(ToolOutput{summary:format!("Phoenix design studio: {label}"),content:serde_json::to_string_pretty(&value)?})
}

fn words(s:&str)->Vec<String>{s.to_lowercase().split(|c:char|!c.is_alphanumeric()).filter(|s|!s.is_empty()).map(str::to_owned).collect()}
fn contains_phrase(text:&str,phrase:&str)->bool {
    let source=words(text);let needle=words(phrase);
    !needle.is_empty()&&source.windows(needle.len()).any(|w|w==needle)
}

fn review(page:Page,blueprint:bool)->Result<Value>{
    for (name,value) in [("product",&page.product),("category",&page.category),("audience",&page.audience),("offer",&page.offer),("primary_action",&page.primary_action)]{bounded(value,name,2000,true)?;}
    if page.sections.is_empty()||page.sections.len()>32{bail!("Use 1–32 content sections, not an unbounded page dump");}
    let mut findings=Vec::new();
    let mut add=|rule:&str,severity:&str,path:String,excerpt:&str,message:&str|findings.push(json!({"rule":rule,"severity":severity,"path":path,"excerpt":excerpt.chars().take(180).collect::<String>(),"message":message}));
    let mut ids=std::collections::BTreeSet::new();let mut labels=std::collections::BTreeMap::<String,std::collections::BTreeSet<String>>::new();
    for (i,section) in page.sections.iter().enumerate(){
        bounded(&section.id,"section id",96,true)?;bounded(&section.heading,"heading",1024,true)?;
        if !section.id.chars().all(|c|c.is_ascii_alphanumeric()||c=='_'||c=='-'){bail!("Section IDs must be simple stable identifiers");}
        if ids.contains(&section.id){bail!("Duplicate section ID: {}",section.id);}
        for (name,values,max) in [("body",&section.body,12),("evidence",&section.evidence,16),("dependencies",&section.dependencies,32)]{
            if values.len()>max{bail!("Too many {name} entries in {}",section.id);}
            for value in values{bounded(value,name,4000,false)?;}
        }
        if section.actions.len()>8{bail!("A section may have at most 8 actions");}
        for action in &section.actions{bounded(&action.label,"action label",160,true)?;bounded(&action.target,"action target",2048,true)?;}
        for (name,value) in [("user_question",&section.user_question),("purpose",&section.purpose),("composition",&section.composition),("compact",&section.compact),("motion",&section.motion)]{bounded(value,name,4000,false)?;}
        if blueprint{
            for (name,value) in [("user_question",&section.user_question),("purpose",&section.purpose),("composition",&section.composition),("compact",&section.compact),("motion",&section.motion)]{
                if value.trim().is_empty(){add("plan/missing-decision","error",format!("sections[{i}].{name}"),"","State the visitor question, section purpose, observed composition, compact transformation and motion decision; static motion is valid.");}
            }
            for dep in &section.dependencies{if !ids.contains(dep){add("plan/dependency-order","error",format!("sections[{i}].dependencies"),dep,"A section may depend only on a preceding section. Reorder the information or remove the false dependency.");}}
        }
        ids.insert(section.id.clone());
        let mut surfaces=vec![(format!("sections[{i}].heading"),section.heading.as_str())];
        surfaces.extend(section.body.iter().enumerate().map(|(j,s)|(format!("sections[{i}].body[{j}]"),s.as_str())));
        for (path,text) in surfaces{
            let generic=["the future of","endless possibilities","where innovation meets","unlock your potential","built for modern teams","game changing","reimagined","seamless","elevate your"];
            if generic.iter().any(|p|contains_phrase(text,p)){add("copy/generic-phrase","warning",path.clone(),text,"Replace the formula with a specific actor, action, object and truthful outcome.");}
            let objective=["trusted by","used by","award winning","number one","most reliable","fastest","guaranteed","clinically proven","act now","limited spots"];
            let ws=words(text);
            let numeric=ws.windows(2).any(|w|w[0].chars().any(|c|c.is_ascii_digit())&&["customers","users","teams","companies","percent","downloads","faster"].contains(&w[1].as_str()))||text.contains('%')||text.contains('$');
            let claim=objective.iter().any(|p|contains_phrase(text,p));
            let illustration=["illustrative","fictional","representative","example"].iter().any(|p|contains_phrase(text,p));
            if claim||(numeric&&!illustration){add("copy/objective-claim",if section.evidence.is_empty(){"error"}else{"review"},path.clone(),text,if section.evidence.is_empty(){"Remove the objective claim or supply genuine section-specific evidence. An invented source string is not proof."}else{"Check the actual source supports this exact claim; the presence of an evidence field does not verify it."});}
            if ws.len()>65{add("copy/dense-block","warning",path.clone(),text,"Shorten this block or split it by the visitor's questions. Keep enough information to understand the product.");}
            if ["lorem ipsum","to be supplied","awaiting approval","insert headline"].iter().any(|p|contains_phrase(text,p)){add("copy/unfinished-placeholder","error",path,text,"Replace authoring placeholders with concrete content. Keep truthful beta, example-data and unavailable-service disclosures.");}
        }
        if words(&section.heading).len()>12{add("copy/heading-length","warning",format!("sections[{i}].heading"),&section.heading,"Check the rendered line count and rewrite a long heading when it impairs scanning. Reference geometry and product meaning take priority.");}
        if i==0&&section.body.len()>1{add("copy/hero-stack","warning",format!("sections[{i}].body"),"","Prefer one clear supporting paragraph below the headline; do not stack several explanations.");}
        for (j,action) in section.actions.iter().enumerate(){
            let normal=words(&action.label).join(" ");
            if ["click here","learn more","read more","discover","explore","start your journey"].contains(&normal.as_str()){add("copy/generic-cta","warning",format!("sections[{i}].actions[{j}]"),&action.label,"Name the action or destination: for example, See Phoenix in action.");}
            if action.target=="#"||action.target.starts_with("javascript:"){add("copy/dead-action","error",format!("sections[{i}].actions[{j}]"),&action.target,"Provide a real route or action. Do not present an unimplemented signup/download as working.");}
            labels.entry(action.target.clone()).or_default().insert(normal);
        }
    }
    for (target,names) in labels{if names.len()>1{add("copy/cta-label-drift","warning",target,&names.into_iter().collect::<Vec<_>>().join(" / "),"Use a consistent label for the same action intent.");}}
    let errors=findings.iter().filter(|r|r["severity"]=="error").count();
    Ok(json!({"mechanical_pass":errors==0,"findings":findings,"requires_rendered_review":true,
        "visitor_contract":{"product":page.product,"category":page.category,"audience":page.audience,"offer":page.offer,"primary_action":page.primary_action},
        "fresh_visitor_review":["Open the actual page at its top without consulting the source or design plan. State what the product is, who it helps, what it does, and the next action using only visible evidence.","Follow the primary action through its real outcome. Inspect what a new visitor would see after a delay, missing input or failure.","Compare desktop and compact screenshots to the intended composition and original brief. Fix opaque copy, weak evidence, repeated template rhythm and broken states, then inspect changed pixels."],
        "limitation":"A lint pass cannot establish comprehension, truth, working interactions or visual quality. This tool does not read evidence files or observe a browser."}))
}

#[cfg(test)]
mod tests{
    use super::*;
    fn page()->Value{json!({"product":"Phoenix","category":"AI desktop app","audience":"People doing computer work","offer":"Build websites and work across files and apps","primary_action":"See Phoenix in action","sections":[{"id":"intro","heading":"AI coworkers for your computer","body":["Phoenix helps you build websites, research questions and work with your files."],"actions":[{"label":"See Phoenix in action","target":"#tour"}]}]})}
    #[test] fn clear_copy_does_not_claim_comprehension(){let value=review(serde_json::from_value(page()).unwrap(),false).unwrap();assert_eq!(value["mechanical_pass"],true);assert_eq!(value["requires_rendered_review"],true);}
    #[test] fn unsupported_claim_and_dead_cta_fail(){let mut p=page();p["sections"][0]["body"]=json!(["Trusted by 50000 teams. Seamless tools for the future of work."]);p["sections"][0]["actions"][0]["target"]=json!("#");let out=review(serde_json::from_value(p).unwrap(),false).unwrap();assert_eq!(out["mechanical_pass"],false);assert!(out["findings"].as_array().unwrap().len()>=3);}
    #[test] fn honest_beta_and_example_disclosures_are_not_deleted(){let mut p=page();p["sections"][0]["body"]=json!(["Private beta for Linux. This example uses test data; no account is connected."]);let out=review(serde_json::from_value(p).unwrap(),false).unwrap();assert_eq!(out["mechanical_pass"],true);}
    #[test] fn blueprint_demands_real_decisions_and_prior_dependencies(){let mut p=page();p["sections"][0]["dependencies"]=json!(["future"]);let out=review(serde_json::from_value(p).unwrap(),true).unwrap();assert_eq!(out["mechanical_pass"],false);assert!(out["findings"].as_array().unwrap().iter().any(|r|r["rule"]=="plan/dependency-order"));}
    #[test] fn unknown_actions_and_oversize_inputs_fail(){assert!(serde_json::from_value::<DesignStudioInput>(json!({"action":"run_shell","command":"bad"})).is_err());assert!(execute(DesignStudioInput::Typography{seed:"x".repeat(1025)}).is_err());}
    #[test] fn typography_is_stable_and_not_one_favorite(){let run=|seed:&str|execute(DesignStudioInput::Typography{seed:seed.into()}).unwrap().content;assert_eq!(run("phoenix"),run("phoenix"));assert_ne!(run("phoenix"),run("print shop"));let v:Value=serde_json::from_str(&run("phoenix")).unwrap();for pool in v["candidates"].as_object().unwrap().values(){assert_eq!(pool.as_array().unwrap().len(),10);}}
}
