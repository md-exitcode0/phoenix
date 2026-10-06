import assert from 'node:assert/strict';
import {citesLine} from './source-citation.mjs';
for(const text of ['`validate_blend.py:61`','`validate_blend.py:34-40, 44, 60-61`','validate_blend.py:60-62'])
    assert(citesLine(text,'validate_blend.py',61));
for(const text of ['validate_blend.py:610','validate_blend.py:34-40, 44, 60','other.py:61','validate_blendXpy:61','validate_blend.py:62-60'])
    assert(!citesLine(text,'validate_blend.py',61));
console.log('SOURCE_CITATION_OK');
