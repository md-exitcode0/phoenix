import {mkdir,readFile,stat,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';

export async function evaluateCsvTool(workspace, evaluation) {
  await mkdir(evaluation,{mode:0o700});
  const positives=[
    ['quoted-unicode-refunds','category,amount\r\n"Café, food",0.10\r\n"Café, food",0.20\r\n"Café, food",-0.05\r\n"two\nlines",12.00\r\n',{'Café, food':'0.25','two\nlines':'12.00'}],
    ['empty','category,amount\n',{}],
    ['large-exact','category,amount\na,999999999999.99\na,0.01\na,-0.10\n',{a:'999999999999.90'}],
    ['reordered','amount,category\n2.3,工具\n',{工具:'2.30'}]
  ];
  const negatives=Object.entries({missing_header:'category\nx\n',duplicate_header:'category,amount,amount\na,1,2\n',extra_field:'category,amount\na,1,unexpected\n',missing_field:'category,amount\na\n',blank_category:'category,amount\n,1\n',malformed_csv:'category,amount\n"unterminated,1\n',nan:'category,amount\na,NaN\n',infinity:'category,amount\na,Infinity\n',invalid:'category,amount\na,ten\n',precision:'category,amount\na,1.234\n',late_failure:'category,amount\na,1.00\nb,bad\n'}).map(([name,csv])=>[name,csv,null]);
  const script=join(workspace,'totals.py');
  const available=await stat(script).then(info=>info.isFile()).catch(error=>{if(error.code==='ENOENT')return false;throw error;});
  const checks=[];
  async function check([name,csv,expected]) {
    // A missing script or one that rejects every input cannot pass rejection
    // tests. Require actual successful valid-input runs before testing errors.
    if(!available||(expected===null&&checks.slice(0,positives.length).some(row=>row.passed!==true))){
      checks.push({name,passed:false,status:'not_evaluated',reason:available?'valid-input controls did not pass':'deliverable not produced',expected});return;
    }
    const input=join(evaluation,name+'.csv'),destination=join(evaluation,name+'.json');
    await writeFile(input,csv);await writeFile(destination,'PREVIOUS OUTPUT\n');
    const run=spawnSync('python3',['-B',script,input,destination],{cwd:workspace,encoding:'utf8',timeout:15000,maxBuffer:1024*1024});
    const actual=await readFile(destination,'utf8').catch(error=>{if(error.code==='ENOENT')return null;throw error;});
    let parsed;try{parsed=JSON.parse(actual)}catch{}
    const equal=expected!==null&&parsed!==null&&typeof parsed==='object'&&!Array.isArray(parsed)
      &&Object.keys(parsed).length===Object.keys(expected).length&&Object.entries(expected).every(([key,value])=>parsed[key]===value);
    const passed=expected!==null?run.status===0&&!!equal
      :run.status!==null&&run.status!==0&&actual==='PREVIOUS OUTPUT\n'&&Boolean(run.stderr?.trim())&&!run.error;
    checks.push({name,passed,status:passed?'passed':'failed',exit:run.status,stderr:run.stderr,executionError:run.error?.message||null,actual,expected});
  }
  for(const entry of [...positives,...negatives])await check(entry);
  return {artifactAvailable:available,checks,passed:checks.filter(row=>row.passed).length,total:checks.length,
    evaluated:checks.filter(row=>row.status!=='not_evaluated').length};
}
