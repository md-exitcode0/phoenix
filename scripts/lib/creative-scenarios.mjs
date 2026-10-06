// Acceptance briefs are ordinary user tasks, not task-specific system guidance.
// New scenarios start in empty workspaces. No observer-written construction or
// implementation is supplied; peer work and visual quality are assessed later.
export const creativeScenarios = Object.freeze({
  banana: Object.freeze({
    kind: 'scene', actor: 'coder', name: 'Banana',
    reference: 'artifacts/banana-reference/banana-reference.jpg',
    prompt: 'Make me a realistic banana in Blender using banana-reference.jpg. Save the editable project and a polished render in this workspace.',
  }),
  website: Object.freeze({
    kind: 'website', actor: 'frontend', name: 'Tideline',
    prompt: '@Iris Build a stunning animated website for TIDELINE, a fictional cold-water surf lodge on Vancouver Island. Give it the character and finish of an independently designed Figma concept, with excellent imagery, typography, responsive layout and thoughtful motion. Work with Theo to find suitable real image assets and source credits, and with Leo to exercise the implemented interactions and report defects. Coordinate their contributions and integrate the result yourself. Include room selection and a local booking planner with arrival, departure and guest count, helpful validation, and a clear local confirmation. The navigation and controls must work on desktop and mobile, and reduced-motion users must remain supported. Keep index.html at the workspace root, include everything needed to run locally and retain image credits. This is a fictional local demo; do not make real bookings or deploy it. Finish the complete site.',
  }),
  observatory: Object.freeze({
    kind: 'website', actor: 'frontend', name: 'Vesper',
    prompt: '@Iris Create a stunning, animated website for VESPER, a fictional mountaintop observatory offering intimate night-sky experiences. It should feel like a bespoke editorial Figma design, not a generic template: beautiful real astronomy imagery, expressive typography, an atmospheric interactive sky, and restrained motion. Work with Theo to find appropriate real assets and source credits, and with Leo to independently exercise the finished interactions. Integrate their work and resolve the defects they find. Visitors must be able to filter three or more experiences, inspect an experience, build a night itinerary, remove selections, see a correct total and revisit their saved itinerary after reloading. Provide a clear local-only confirmation and a reset option. Use fictional demo dates and availability, not claims about current astronomical events or real bookings. Make every control useful on desktop and mobile, with keyboard access and reduced-motion support. Deliver index.html and all required local files, with credits. Do not deploy, purchase anything or contact real venues. Finish the whole site.',
  }),
  festival: Object.freeze({
    kind: 'website', actor: 'frontend', name: 'Switchyard',
    prompt: '@Iris Build an extraordinary animated website for SWITCHYARD, a fictional two-day experimental music and moving-image festival. Make it feel like a kinetic festival poster that becomes a useful timetable: bold graphic typography, playful colour, tightly composed information and a distinctive sense of rhythm. The previous sites repeated a full-screen scenic photograph, elegant italic serif headlines and long spacious editorial sections; this must be a genuinely different design, not that template with new colours. Work with Theo to source suitable real photographs or artwork with accurate credits, and with Leo to independently test the implemented interactions. Integrate their returns and finish the site yourself. Include at least eight fictional events across two days and three stages, day/stage filters, event details and a personal schedule that persists on reload. Adding overlapping events must explain the conflict; visitors can remove events and clear their plan. Keep all dates, artists and venues clearly fictional, with no real tickets, payments or bookings. Every control must work on desktop, mobile and keyboard, with a reduced-motion alternative. Deliver index.html and all required local files and credits. Do not deploy or contact anyone.',
  }),
  printshop: Object.freeze({
    kind: 'website', actor: 'frontend', name: 'Overprint',
    prompt: '@Iris Make a remarkable interactive website for OVERPRINT, a fictional independent print workshop. The central experience is a tactile print-making workbench, with a light paper surface, a compact specimen catalogue and a live two-ink poster preview. Give it its own visual character and motion: no full-screen scenic-photo opening, oversized elegant serif headline or long airy landing-page template. Work with Theo to find real printmaking imagery and reusable assets with source credits, and with Leo to test the working site independently. Integrate their contributions and resolve their findings. Visitors must be able to choose one of at least six original poster compositions, change both inks and paper, see the preview update, set quantity, and see a consistent itemised demo price. Preserve their configuration after reload, support reset and provide a clearly local-only proof confirmation. Label the workshop and prices as fictional; no payments, uploads to a service or real orders. Make it useful on mobile and desktop with keyboard access and reduced-motion support. Deliver index.html, all local dependencies and credits. Do not deploy or contact anyone.',
  }),
  lamp: Object.freeze({
    kind: 'scene', actor: 'coder', name: 'Sculptural lamp',
    prompt: 'Create a photorealistic studio presentation of an elegant sculptural desk lamp in Blender: warm brushed brass, an arched stem, a pleated ivory linen shade and a dark stone base. It should look like a convincing finished designer object, with the lamp lit. Save the editable project and a polished render in this workspace.',
  }),
});

export function creativeScenario(name) {
  if (!Object.hasOwn(creativeScenarios, name)) throw new Error(`Unknown creative scenario: ${name}`);
  return creativeScenarios[name];
}

// These are presence/coordination gates only. Actual output must be inspected
// independently; a successful process or the presence of files is not quality.
export function creativeArtifactChecks(recipe, files) {
  if (recipe.kind === 'scene') return {
    editableScene: files.some(file => file.path.endsWith('.blend') && file.bytes > 10000),
    renderedOutput: files.some(file => file.path.endsWith('.png') && file.bytes > 10000),
  };
  return {indexExists: files.some(file => file.path === 'index.html' && file.bytes > 1000)};
}
