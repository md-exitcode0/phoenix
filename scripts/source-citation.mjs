// Recognize a cited line in a filename-qualified line/range list.
export function citesLine(markdown, filename, line) {
    const escaped=filename.replace(/[.*+?^${}()|[\]\\]/g,'\\$&');
    const expression=new RegExp(`${escaped}:(\\d+(?:-\\d+)?(?:,\\s*\\d+(?:-\\d+)?)*)`,'g');
    return [...markdown.matchAll(expression)].some(match=>match[1].split(',').some(part=>{
        const [start,end]=part.trim().split('-').map(Number);
        return start<=line && line<=(end??start);
    }));
}
