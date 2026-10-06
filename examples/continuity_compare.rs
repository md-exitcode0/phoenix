//! Paired continuity probe over an actual completed task transcript.
//! Both modes retain the same archive; neither creator sees the later questions.
use std::{path::Path, sync::Arc, time::Instant};
use anyhow::{Context, Result};
use phoenix_agent::{config::PhoenixConfig, providers::{ProviderFactory, LLMProvider, CompletionRequest, ChatMessage}, session::Session};
use serde_json::json;

const MODEL:&str="gpt-5.6-sol";

async fn answer(provider:&Arc<dyn LLMProvider>,context:String,questions:&str,archive:&Path,id:&str)->Result<serde_json::Value>{
    let mut messages=vec![ChatMessage::system("Answer questions about the prior real task using the supplied continuation and its searchable archive. Use recall for missing exact evidence. Distinguish requested output, claimed success and verified output. Do not invent facts. Finish with final_answer. You have no file, shell or external-service tools."),ChatMessage::user(context),ChatMessage::user(questions)];
    let mut tools=phoenix_agent::tools::tool_definitions_for_agent(&["recall".into(),"final_answer".into()]);
    tools.retain(|tool|matches!(tool.name.as_str(),"recall"|"final_answer"));
    let started=Instant::now();let mut events=Vec::new();let mut usage=0u64;
    for round in 0..12 {
        let mut request=CompletionRequest::new(MODEL,messages.clone());
        request.tools=tools.clone();request.extra_body.insert("reasoning".into(),json!({"effort":"medium"}));
        let response=provider.complete(request).await?;
        anyhow::ensure!(response.model==MODEL,"provider returned unexpected model {}",response.model);
        usage+=u64::from(response.usage.input_tokens)+u64::from(response.usage.output_tokens);
        let calls=response.tool_calls;
        anyhow::ensure!(!calls.is_empty(),"continuation returned no tool call");
        messages.push(ChatMessage::assistant_tool_calls(response.content,calls.clone()));
        for call in calls {
            if call.tool_name=="final_answer" {
                return Ok(json!({"rounds":round+1,"elapsed_ms":started.elapsed().as_millis(),"reported_tokens":usage,
                    "events":events,"final":call.arguments}));
            }
            anyhow::ensure!(call.tool_name=="recall","unavailable tool requested");
            let result=phoenix_agent::tools::recall::execute(serde_json::from_value(call.arguments.clone())?,archive,id);
            let (success,output)=match result {Ok(out)=>(true,out.content),Err(error)=>(false,error.to_string())};
            events.push(json!({"input":call.arguments,"success":success,"output":output}));
            messages.push(ChatMessage::tool_result(call.id,output));
        }
    }
    anyhow::bail!("continuation exhausted its twelve-round evaluation budget")
}

#[tokio::main]
async fn main()->Result<()> {
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!((3..=4).contains(&args.len()),"source session JSON, question file, NEW output directory required");
    let original=std::fs::read(&args[0])?;
    let source:Session=serde_json::from_slice(&original)?;
    let output=Path::new(&args[2]);std::fs::create_dir(output)?;
    let config=PhoenixConfig::load()?;
    anyhow::ensure!(config.profile.llm.model==MODEL && config.profile.llm.reasoning_effort.as_deref()==Some("medium"),"probe requires Sol medium");
    anyhow::ensure!(config.profile.llm.efforts.get("librarian").map(String::as_str)==Some("medium"),"compaction librarian must also use medium");
    let provider=ProviderFactory::new().build_llm_provider(&config.profile.llm)?;
    let mut baseline=Session::new_main_with_id("continuity",MODEL,"Continue the original task faithfully.");
    baseline.replace_messages(source.messages.clone());
    let archive=output.join("archive");std::fs::create_dir(&archive)?;
    let split=phoenix_agent::runtime::compaction::split_point(&source.messages,
        phoenix_agent::runtime::compaction::keep_tokens_for_window(16000) as usize*4);
    let (notes_text,summary_prepare,notes_prepare)=if let Some(prepared)=args.get(3) {
        let prepared=Path::new(prepared);
        let expected=source.messages.iter().map(serde_json::to_string).collect::<std::result::Result<Vec<_>,_>>()?.join("\n")+"\n";
        anyhow::ensure!(std::fs::read_to_string(prepared.join("archive/continuity.archive.jsonl"))?==expected,"prepared archive differs from source");
        baseline=serde_json::from_slice(&std::fs::read(prepared.join("summary-session.json"))?)?;
        let prior:serde_json::Value=serde_json::from_slice(&std::fs::read(prepared.join("comparison.json"))?)?;
        anyhow::ensure!(prior["split"].as_u64()==Some(split as u64),"prepared recent-tail split differs");
        let notes=std::fs::read_to_string(prepared.join("working-notes.md"))?;
        (notes,json!({"reused":true,"elapsed_ms":0,"input_tokens":0,"output_tokens":0}),json!({"reused":true,"elapsed_ms":0,"input_tokens":0,"output_tokens":0}))
    } else {
    let start=Instant::now();
    let result=phoenix_agent::runtime::compaction::compact_session(&provider,MODEL,&mut baseline,16000,Some(&archive)).await.context("production compaction did not run")?;
    let summary_prepare=json!({"elapsed_ms":start.elapsed().as_millis(),"before_chars":result.before_chars,
        "after_chars":result.after_chars,"used_model":result.used_model,"mechanical_only":result.mechanical_only,
        "input_tokens":result.summarizer_input_tokens,"output_tokens":result.summarizer_output_tokens});
    let start=Instant::now();
    let mut request=CompletionRequest::new(MODEL,vec![ChatMessage::system("Write working notes for an agent resuming this actual task after a context reset. Preserve the objective, constraints, failures and why they failed, decisions, exact artifact paths, what is verified, and unfinished work. Prioritize useful continuity over narration. The full transcript remains searchable with recall; reference what can be retrieved. At most 1200 words. Do not answer hypothetical future questions or claim unverified success."),ChatMessage::user(serde_json::to_string(&source.messages[..split])?)]);
    request.max_tokens=Some(6000);request.extra_body.insert("reasoning".into(),json!({"effort":"medium"}));
    let notes=provider.complete(request).await?;
    anyhow::ensure!(notes.model==MODEL,"notes provider returned another model");
    let notes_prepare=json!({"elapsed_ms":start.elapsed().as_millis(),"chars":notes.content.chars().count(),"input_tokens":notes.usage.input_tokens,"output_tokens":notes.usage.output_tokens});
        (notes.content,summary_prepare,notes_prepare)
    };
    std::fs::write(output.join("summary-session.json"),serde_json::to_vec_pretty(&baseline)?)?;
    std::fs::write(output.join("working-notes.md"),&notes_text)?;
    let mut notes_session=baseline.clone();
    let mut notes_messages=vec![phoenix_agent::session::Message::User {
        content:format!("Working notes from the preceding context (model-authored; verify claims with recall):\n{notes_text}")
    }];
    notes_messages.extend_from_slice(&source.messages[split..]);
    notes_session.replace_messages(notes_messages);
    let tail=&source.messages[split..];
    anyhow::ensure!(baseline.messages.len()>=tail.len() &&
        serde_json::to_value(&baseline.messages[baseline.messages.len()-tail.len()..])?==serde_json::to_value(tail)?,
        "production compaction retained a different recent tail; comparison is not equivalent");
    std::fs::write(output.join("notes-session.json"),serde_json::to_vec_pretty(&notes_session)?)?;
    // Normalize archive access: both modes can search every original record,
    // including the identical retained recent tail. No baseline history loss.
    let raw=source.messages.iter().map(serde_json::to_string).collect::<std::result::Result<Vec<_>,_>>()?.join("\n")+"\n";
    std::fs::write(archive.join("continuity.archive.jsonl"),raw)?;
    if args[1]=="--prepare-only" {
        anyhow::ensure!(std::fs::read(&args[0])?==original,"source transcript changed during preparation");
        std::fs::write(output.join("comparison.json"),serde_json::to_vec_pretty(&json!({
            "mode":"prepare-only","model":MODEL,"reasoning_effort":"medium",
            "source_messages":source.messages.len(),"split":split,
            "summary_prepare":summary_prepare,"notes_prepare":notes_prepare,
            "summary_context_chars":serde_json::to_string(&baseline.messages)?.chars().count(),
            "notes_context_chars":serde_json::to_string(&notes_session.messages)?.chars().count(),
            "source_unchanged":true,
            "limits":"Prepared contexts only. No resumed task has run and no comparative advantage is established."
        }))?)?;
        return Ok(());
    }
    let questions=std::fs::read_to_string(&args[1])?;
    let summary_context=serde_json::to_string(&baseline.messages)?;
    let notes_context=format!("Working notes (model-authored; verify claims):\n{}\nRecent original messages:\n{}",notes_text,serde_json::to_string(&source.messages[split..])?);
    let summary=answer(&provider,summary_context.clone(),&questions,&archive,"continuity").await?;
    std::fs::write(output.join("summary-answer.json"),serde_json::to_vec_pretty(&summary)?)?;
    let notes_answer=answer(&provider,notes_context.clone(),&questions,&archive,"continuity").await?;
    std::fs::write(output.join("notes-answer.json"),serde_json::to_vec_pretty(&notes_answer)?)?;
    anyhow::ensure!(std::fs::read(&args[0])?==original,"source transcript changed during probe");
    std::fs::write(output.join("comparison.json"),serde_json::to_vec_pretty(&json!({
        "model":MODEL,"reasoning_effort":"medium","source_messages":source.messages.len(),"split":split,
        "summary_prepare":summary_prepare,"notes_prepare":notes_prepare,
        "summary_context_chars":summary_context.chars().count(),"notes_context_chars":notes_context.chars().count(),
        "summary":summary,"notes":notes_answer,"source_unchanged":true,"prepared_contexts_reused":args.get(3).is_some(),
        "limits":"One paired continuation over real task history, not a long-session production rollout. Answers require independent rubric review; no superiority claim is automatic. Fixed summary-first order can affect caching and timings."
    }))?)?;
    Ok(())
}
