/* Clearly isolated durable test fixture. Never a Tauri/backend acceptance claim. */
(function(root){
 'use strict';
 if(root.__PHOENIX_ISOLATED_BACKEND__?.enabled)return;
 if(root.__TAURI__)throw Error('Fluffies preview must not connect to the production bridge');
 const state={profiles:{},images:{},commands:[]};
 async function load(){const r=await fetch('/fixture-state');if(!r.ok)throw Error('Could not load isolated Configure fixture');Object.assign(state,await r.json());return state}
 const ready=load();
 async function post(value){const r=await fetch('/fixture-directory',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(value)});const v=await r.json();if(!r.ok)throw Error(v.error||'Fixture save failed');return v}
 function restore(view){for(const agent of view.directory.agents){const saved=state.profiles[agent.agent_id];if(saved){for(const key of ['display_name','role_title','description','color','icon_seed'])if(saved[key]!=null)agent[key]=saved[key];let metadata={};try{metadata=JSON.parse(agent.metadata_json||'{}')}catch{}if(saved.avatar)metadata.avatar=saved.avatar;agent.metadata_json=JSON.stringify(metadata)}}return view}
 root.PhoenixFluffyFixture=Object.freeze({ready,restore,async save(command){await post({CompanyDirectory:command});await load()},async importImage(dataUrl){const value=await post({FixtureImage:{dataUrl}});await load();return value.imageId},image(id){return state.images[id]||null},snapshot(){return structuredClone(state)}});
})(globalThis);
