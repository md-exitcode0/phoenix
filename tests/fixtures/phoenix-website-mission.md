@everyone New mission, and it's a big one: the public website for Phoenix at usephoenix.dev. It's my first production website and I want it to be the best one any of you has ever made. @Tibo you lead. Run it as diverge → research → converge → build, and get EVERYONE involved: Theo, Robin, Leon, Remy, Rory.

Before building, read this whole brief and make sure you understand what I mean, not just what I wrote. If anything is unclear, ask me in the room first. Don't guess.

@Leon: you own the design. I'm not giving you a layout on purpose. You're the pro. Below is what each section must achieve and how it should feel. How it looks is your call. Dig through your component libraries and make it the best thing you've ever made.

WHAT PHOENIX IS
A desktop app where you run a team of AI coworkers that live on your computer. Each one has a role, its own memory, its own browser, logins and tools. They work together in group rooms: everyone hears everything, you @mention who should act, and a leader plans and runs the work. Phoenix can do almost anything for you.

THE PAGE, TOP TO BOTTOM

0. Nav: logo, a few links, and a main "Coming soon" button.

1. HERO: "Rebirth", a scroll-driven Blender film starring our four fluffies (Round, Triangle, Diamond, Cube, from their real Blender sources, with their real palettes, eyes and props).
- Wake: darkness and ash, then an ember pops into the phoenix, which swoops over four sleeping fluffies. They blink awake.
- Jump: a staggered cascade of jumps, each in its own style (Round bounces, Triangle flips, Diamond spins, Cube stomp-hops and shakes the camera). They land at glowing laptops.
- Code: focused eyes, paws blurring, code rising as light ribbons.
- Play while working: they never stop working, but it's joyful chaos. They bounce between laptops typing mid-air, toss code blocks like balls, squish a red bug until it turns green, leapfrog and high-five.
- Ship: they stack into a tower, the phoenix lands on top, the code launches like a rocket, everyone winks, and the camera pulls back into the Phoenix logo.
Use big squash and stretch, anticipation, overshoot and fur jiggle. Think Pixar short. Whenever scrolling stops, they keep breathing, blinking and doing tiny hops. The hero line, the "Coming soon" button and a WORKING email waitlist sit over the film.

2. PICK YOUR FLUFFY: the real 3D fluffies, not icons or shapes. When you pick one, it plays its own unique happy animation: jumping, swirling, squishing, sparkles, so it's obvious they're thrilled you chose them. Each fluffy gets a different celebration. Colours must be intuitive: a beautiful live palette where the fluffy itself changes colour as you hover, with friendly colour names. No grid of empty circles.

3. WHAT IT CAN DO: horizontal scroll cards (group rooms, own browsers, logins and password vault, connectors, self-checking builds, motion graphics). Clicking a card smoothly expands it, and the page glides down a little so you can read the detail. Inside each card: REAL screenshots from the Monocode version of the app, close-ups of the browser plus the full app in context. Make them smart, beautiful product shots: framed, cropped, layered, maybe subtly animated. Never raw screen dumps. @Robin and @Leon run the real app and capture them.
Then close the section with a compelling, beautifully designed conclusion that makes people want it. Leon, you know how.

4. WHAT PEOPLE SAY: review and testimonial cards in a great-looking layout from your component libraries. Use lorem ipsum placeholder text and avatars FOR NOW. Mark every one clearly as a placeholder in the code (data-placeholder="true") so we swap in real quotes before launch. Never present them as real.

5. PHOENIX CAN DO ANYTHING: the showstopper. Invent a super cool, intuitive UI that explains it. It must land these points:
- The Rebirth animation and the fluffies were one-shotted by Phoenix itself.
- This whole website was one-shotted by Phoenix too.
- It uses your logins, browses, codes, designs, researches and ships while you live your life.
- Then one short, confident claim: Phoenix is better than any personal superintelligence agent you've ever used.

6. Final "Coming soon" call to action with the waitlist, then the footer: © Phoenix, GitHub, contact mikd6767@usephoenix.dev.

THE WAITLIST MUST ACTUALLY WORK
The email signup is real, not a mockup. The site is hosted on Cloudflare Pages: use a Pages Function plus a D1 database to store signups, validate emails, block duplicates, and add a honeypot plus rate limiting against spam. Send a real "you're on the list" confirmation email through a transactional email provider with a free tier. Pick one and tell me what account it needs. My dad will open that account, since most providers require 18+, so list exactly what he has to do. Keep the API keys in environment secrets, never in the code. Add a simple way for me to export the list.

TECH AND QUALITY
- Build it as Astro or a static site, with three.js and GSAP ScrollTrigger for the film and the 3D fluffies.
- Export from Blender as glTF (meshopt or Draco, KTX2 textures, fur baked), under 8 MB for the hero.
- Phones and low-power devices get a WebP frame-sequence fallback. Reduced motion gets stills.
- Hold 60 fps on a laptop and score 85 or better on Lighthouse mobile. Make it fully responsive and accessible.
- Use motion graphics generously, but every animation has to feel intentional.

WHO DOES WHAT
- Diverge: Leon, Theo and Robin each pitch a direction on their own, without seeing the others. Theo studies the best scroll-animation and dev-tool launch sites he can find.
- Converge: Tibo red-teams the pitches and posts the decision. The storyboard, the fluffies and the section goals stay. Everything visual is open.
- Build: Leon designs and animates, Robin builds the site and the waitlist backend, and Theo writes and sharpens the copy.
- Remy security-reviews the waitlist, the secrets and the screenshots. Nothing personal shows: no real logins, emails, tokens or private chats.
- Rory drafts the launch post with a 15-second vertical cut of the play beat. Don't publish anything.
- Leon posts contact sheets of the film beats and of the fluffy celebrations to the room before final renders.

DELIVER: a repo ready for Cloudflare Pages, the .blend sources, the waitlist setup steps, and screenshots plus a screen recording at desktop and mobile widths.
