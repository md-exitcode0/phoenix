import {homedir} from 'node:os';
import {isAbsolute,join,resolve} from 'node:path';
import {realpathSync,statSync} from 'node:fs';

// Live acceptance fixtures must never create rooms in the user's company.
export function isolatedTestHome(value = process.env.PHOENIX_HOME) {
  if (!value || !isAbsolute(value)) throw Error('Set PHOENIX_HOME to an isolated test gateway home before running this acceptance script.');
  const home = realpathSync(value);
  const personal = join(homedir(), '.phoenix');
  const sameDirectory = (a,b) => {
    try { const x=statSync(a),y=statSync(b); return x.dev===y.dev && x.ino===y.ino; }
    catch { return false; }
  };
  if (home === resolve(personal) || sameDirectory(home,personal)
      || sameDirectory(join(home,'gateway.sock'),join(personal,'gateway.sock'))
      || sameDirectory(join(home,'company/company.sqlite'),join(personal,'company/company.sqlite'))) {
    throw Error('Acceptance scripts cannot use the personal Phoenix company. Start an isolated test gateway.');
  }
  return home;
}
