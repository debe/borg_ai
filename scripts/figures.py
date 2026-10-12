"""Writes the README figures to docs/img as standalone SVGs: own background, light and dark palettes, no web fonts.

Run: python3 scripts/figures.py [OUT_DIR]"""
import math, pathlib, sys

OUT = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parent.parent / "docs" / "img")
OUT.mkdir(parents=True, exist_ok=True)

STYLE = """<style>
.bg{fill:#ffffff;stroke:#d5d9e0}
text{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;fill:#12161c}
.d{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI","Helvetica Neue",Arial,sans-serif;font-weight:700}
.m{fill:#5a6371}.a{fill:#1f49c7}.h{fill:#b8471f}
.box{fill:#ffffff;stroke:#d5d9e0;stroke-width:1.2}
.box-a{fill:#e2e8fa;stroke:#1f49c7;stroke-width:1.4}
.box-h{fill:#f6e4dc;stroke:#b8471f;stroke-width:1.4}
.ln{stroke:#5a6371;stroke-width:1.3;fill:none}
.ln-a{stroke:#1f49c7;stroke-width:1.7;fill:none}
.dash{stroke-dasharray:5 5}
.grid{stroke:#d5d9e0;stroke-width:1}
.f-a{fill:#1f49c7}.f-h{fill:#b8471f}.f-m{fill:#5a6371;opacity:.45}.f-g{fill:#1f7a4d}
.cell{fill:#ffffff;stroke:#d5d9e0;stroke-width:.8}
.lit{fill:#1f49c7;stroke:#d5d9e0;stroke-width:.8}
.lit-h{fill:#b8471f;stroke:#d5d9e0;stroke-width:.8}
@media (prefers-color-scheme:dark){
.bg{fill:#141922;stroke:#272f3b}
text{fill:#e3e7ee}
.m{fill:#8d97a7}.a{fill:#8aa6ff}.h{fill:#f08a5d}
.box{fill:#141922;stroke:#272f3b}
.box-a{fill:#18213a;stroke:#8aa6ff}
.box-h{fill:#2c1a12;stroke:#f08a5d}
.ln{stroke:#8d97a7}.ln-a{stroke:#8aa6ff}.grid{stroke:#272f3b}
.f-a{fill:#8aa6ff}.f-h{fill:#f08a5d}.f-m{fill:#8d97a7}.f-g{fill:#5fc994}
.cell{fill:#141922;stroke:#272f3b}.lit{fill:#8aa6ff;stroke:#272f3b}.lit-h{fill:#f08a5d;stroke:#272f3b}
}
</style>"""


def svg(name, w, h, label, body):
    doc = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" aria-label="{label}">\n'
           f'<title>{label}</title>\n{STYLE}\n<rect x="0.5" y="0.5" width="{w-1}" height="{h-1}" rx="6" class="bg"/>\n{body}\n</svg>\n')
    (OUT / name).write_text(doc)


def cube():
    n, s, cx, cy = 6, 26, 220, 200
    lit = {"t-1-3": "a", "t-4-1": "a", "l-2-4": "a", "l-4-2": "h", "r-1-1": "a", "r-3-4": "a", "r-5-2": "a", "t-2-5": "a"}
    ix, iy = s * math.cos(math.pi / 6), s * math.sin(math.pi / 6)
    out, centres = [], []
    pt = lambda x, y: f"{x:.1f},{y:.1f}"
    cls = lambda c: "lit-h" if c == "h" else ("lit" if c else "cell")
    for a in range(n):
        for b in range(n):
            tx, ty = cx + (a - b) * ix, cy - n * s + (a + b) * iy
            c = lit.get(f"t-{a}-{b}")
            out.append(f'<polygon points="{pt(tx,ty)} {pt(tx+ix,ty+iy)} {pt(tx,ty+2*iy)} {pt(tx-ix,ty+iy)}" class="{cls(c)}"/>')
            if c: centres.append((tx, ty + iy))
            lx, ly = cx - n * ix + a * ix, cy - n * s + n * iy + a * iy + b * s
            c = lit.get(f"l-{a}-{b}")
            out.append(f'<polygon points="{pt(lx,ly)} {pt(lx+ix,ly+iy)} {pt(lx+ix,ly+iy+s)} {pt(lx,ly+s)}" class="{cls(c)}"/>')
            if c: centres.append((lx + ix / 2, ly + iy / 2 + s / 2))
            rx, ry = cx + a * ix, cy - n * s + 2 * n * iy - a * iy + b * s
            c = lit.get(f"r-{a}-{b}")
            out.append(f'<polygon points="{pt(rx,ry)} {pt(rx+ix,ry-iy)} {pt(rx+ix,ry-iy+s)} {pt(rx,ry+s)}" class="{cls(c)}"/>')
            if c: centres.append((rx + ix / 2, ry - iy / 2 + s / 2))
    for x, y in centres:
        out.append(f'<line x1="{x:.1f}" y1="{y:.1f}" x2="{cx}" y2="{cy-10}" class="ln-a" opacity=".55"/>')
    out.append(f'<circle cx="{cx}" cy="{cy-10}" r="9" class="f-a"/>')
    out.append(f'<text x="{cx}" y="388" text-anchor="middle" class="m" font-size="11">drones on every face · one mind at the core</text>')
    svg("cube.svg", 440, 400, "An isometric cube of cells; lit cells are drones linked to one mind at the core", "\n".join(out))


cube()

svg("one-correction.svg", 1000, 300, "A correction becomes a lesson, changes another drone's work, reaches a colleague over the mesh and becomes long-term memory", """
<line x1="40" y1="64" x2="960" y2="64" class="grid"/>
<circle cx="130" cy="64" r="7" class="f-h"/><text x="130" y="40" text-anchor="middle" class="d" font-size="15">Monday</text>
<rect x="30" y="92" width="200" height="180" rx="4" class="box-h"/>
<text x="44" y="118" class="d" font-size="13">The engineer corrects</text>
<text x="44" y="140" class="m" font-size="11">"endpoints aren't secrets,</text><text x="44" y="156" class="m" font-size="11">they go in config,</text><text x="44" y="172" class="m" font-size="11">not in Vault"</text>
<text x="44" y="198" font-size="11">Borg learns a lesson:</text>
<text x="44" y="214" class="m" font-size="11">question → consequence</text>
<text x="44" y="244" class="m" font-size="10.5">session end: one model call</text>
<circle cx="370" cy="64" r="7" class="f-a"/><text x="370" y="40" text-anchor="middle" class="d" font-size="15">Tuesday</text>
<rect x="270" y="92" width="200" height="180" rx="4" class="box-a"/>
<text x="284" y="118" class="d" font-size="13">Another repo, another drone</text>
<text x="284" y="140" class="m" font-size="11">plans to put an endpoint</text><text x="284" y="156" class="m" font-size="11">in Vault</text>
<text x="284" y="180" font-size="11">Borg asks: "is it secret?"</text>
<text x="284" y="198" class="m" font-size="11">the drone uses config</text>
<text x="284" y="224" class="a" font-size="11">judged: changed, avoids it</text>
<text x="284" y="242" class="a" font-size="11">→ the lesson gets stronger</text>
<circle cx="610" cy="64" r="7" class="f-a"/><text x="610" y="40" text-anchor="middle" class="d" font-size="15">Wednesday</text>
<rect x="510" y="92" width="200" height="180" rx="4" class="box"/>
<text x="524" y="118" class="d" font-size="13">A colleague's laptop</text>
<text x="524" y="140" class="m" font-size="11">the lesson arrived over the</text><text x="524" y="156" class="m" font-size="11">peer mesh, no server</text>
<text x="524" y="186" font-size="11">used again, by another</text><text x="524" y="202" font-size="11">engineer, in another repo</text>
<text x="524" y="236" class="m" font-size="10.5">uses are shared log lines</text>
<circle cx="850" cy="64" r="7" class="f-g"/><text x="850" y="40" text-anchor="middle" class="d" font-size="15">Weeks later</text>
<rect x="750" y="92" width="220" height="180" rx="4" class="box"/>
<text x="764" y="118" class="d" font-size="13">Long-term memory</text>
<text x="764" y="140" class="m" font-size="11">delivered as soon as config</text><text x="764" y="156" class="m" font-size="11">work starts, before a plan</text>
<text x="764" y="186" font-size="11">a lesson nobody acts on</text><text x="764" y="202" font-size="11">fades and goes dormant;</text>
<text x="764" y="218" font-size="11">one strong match wakes it</text>""")

svg("turn.svg", 1000, 330, "One turn: free early delivery, a free word gate at the turn end, one graded model call, an extra round only for a grade-3 lesson", """
<rect x="20" y="40" width="190" height="130" rx="4" class="box-a"/>
<text x="34" y="66" class="d" font-size="14">1 · Prompt or first touch</text>
<text x="34" y="90" class="m" font-size="11">the drone names or opens</text><text x="34" y="106" class="m" font-size="11">a lesson's topic</text>
<text x="34" y="132" font-size="11">its lessons, one line each</text>
<text x="34" y="152" class="a" font-size="11">free · no extra round</text>
<line x1="210" y1="105" x2="250" y2="105" class="ln-a"/>
<rect x="250" y="40" width="150" height="130" rx="4" class="box"/>
<text x="264" y="66" class="d" font-size="14">2 · The drone works</text>
<text x="264" y="90" class="m" font-size="11">hooks note the</text><text x="264" y="106" class="m" font-size="11">repo and touched</text><text x="264" y="122" class="m" font-size="11">paths: its scope</text>
<line x1="400" y1="105" x2="440" y2="105" class="ln-a"/>
<rect x="440" y="40" width="150" height="130" rx="4" class="box"/>
<text x="454" y="66" class="d" font-size="14">3 · Free gate</text>
<text x="454" y="90" class="m" font-size="11">does any lesson's</text><text x="454" y="106" class="m" font-size="11">wording match?</text>
<text x="454" y="140" class="a" font-size="11">no → done, 0 tokens</text>
<line x1="590" y1="105" x2="630" y2="105" class="ln-a"/>
<rect x="630" y="20" width="350" height="170" rx="4" class="box-a"/>
<text x="644" y="46" class="d" font-size="14">4 · One model call (Sonnet, ~10 s)</text>
<text x="644" y="70" font-size="11">reads every lesson that passes the</text>
<text x="644" y="86" font-size="11">system gate, one line each</text>
<text x="644" y="112" class="m" font-size="11">a · which apply? graded 3 · 2 · 1</text>
<text x="644" y="130" class="m" font-size="11">b · did earlier questions change the</text><text x="644" y="146" class="m" font-size="11">    work and avoid their consequence?</text>
<text x="644" y="174" class="a" font-size="11">b rides along: no extra call</text>
<line x1="805" y1="190" x2="805" y2="226" class="ln-a"/>
<rect x="440" y="226" width="540" height="88" rx="4" class="box-h"/>
<text x="454" y="252" class="d" font-size="14">5 · Block only for a grade 3, then ask 3s, 2s, 1s (≤10)</text>
<text x="454" y="274" class="m" font-size="11">one extra round, the expensive part · each lesson once per session</text>
<text x="454" y="294" class="m" font-size="11">no grade 3 → no extra round; 2s and 1s arrive free at the next prompt or touch</text>
<rect x="20" y="226" width="380" height="88" rx="4" class="box"/>
<text x="34" y="252" class="d" font-size="14">Every hook fails open</text>
<text x="34" y="274" class="m" font-size="11">any error, missing model or index: the drone</text><text x="34" y="290" class="m" font-size="11">just continues. Hooks take a few ms; vectors</text><text x="34" y="306" class="m" font-size="11">~0.1 s, only in step 4.</text>""")

svg("memory.svg", 480, 300, "Activation of a lesson over sessions: rises at each use, decays between uses, spaced uses keep it above the dormancy line", """
<line x1="40" y1="250" x2="460" y2="250" class="grid"/><line x1="40" y1="30" x2="40" y2="250" class="grid"/>
<line x1="40" y1="200" x2="460" y2="200" class="ln dash"/><text x="456" y="194" text-anchor="end" class="m" font-size="10">dormant below</text>
<path d="M40 230 L70 120 C90 150 110 170 140 178 L150 100 C175 130 205 150 250 160 L260 82 C300 110 350 128 400 140 L410 70 C430 90 445 100 460 106" class="ln-a"/>
<circle cx="70" cy="120" r="4" class="f-a"/><circle cx="150" cy="100" r="4" class="f-a"/><circle cx="260" cy="82" r="4" class="f-a"/><circle cx="410" cy="70" r="4" class="f-a"/>
<text x="70" y="108" text-anchor="middle" class="m" font-size="10">use</text>
<text x="410" y="58" text-anchor="middle" class="m" font-size="10">another engineer</text>
<text x="250" y="274" text-anchor="middle" class="m" font-size="11">sessions of the collective →</text>
<text x="30" y="40" text-anchor="end" class="m" font-size="10" transform="rotate(-90 30 40)">activation</text>""")

svg("architecture.svg", 1000, 260, "Layers: harness hooks, the borg CLI, the local store and the turn-end check, the peer mesh, team trust", """
<rect x="16" y="14" width="968" height="40" rx="3" class="box"/><text x="32" y="39" class="d" font-size="14">Harness hooks</text><text x="330" y="39" class="m" font-size="11.5">Claude Code · omp · opencode → one event set (prompt, tool, failure, stop, end)</text>
<rect x="16" y="62" width="968" height="40" rx="3" class="box-a"/><text x="32" y="87" class="d" font-size="14">borg CLI</text><text x="330" y="87" class="m" font-size="11.5">one process per hook, a few ms, fails open; capture costs no tokens</text>
<rect x="16" y="110" width="478" height="40" rx="3" class="box"/><text x="32" y="135" class="d" font-size="14">Local store</text><text x="160" y="135" class="m" font-size="11">append-only JSONL per collective</text>
<rect x="506" y="110" width="478" height="40" rx="3" class="box"/><text x="522" y="135" class="d" font-size="14">Turn-end check</text><text x="680" y="135" class="m" font-size="11">free gate → one graded model call</text>
<rect x="16" y="158" width="968" height="40" rx="3" class="box"/><text x="32" y="183" class="d" font-size="14">Peer mesh</text><text x="330" y="183" class="m" font-size="11.5">iroh QUIC, signed membership, set reconciliation, no leader, no model calls</text>
<rect x="16" y="206" width="968" height="40" rx="3" class="box-h"/><text x="32" y="231" class="d" font-size="14">Team trust</text><text x="330" y="231" class="m" font-size="11.5">identity keys, signed steering, trust per engineer × area, redaction at origin</text>""")

svg("escalation.svg", 1000, 300, "Escalation: memory, an expert repo, a live drone, the engineer; each step only when the one before cannot answer", """
<rect x="20" y="40" width="220" height="200" rx="4" class="box-a"/><text x="34" y="66" class="d" font-size="14">1 · Memory</text>
<text x="34" y="90" class="m" font-size="11">lessons of this collective,</text><text x="34" y="106" class="m" font-size="11">graded by the model at the</text><text x="34" y="122" class="m" font-size="11">turn end, free early delivery</text>
<text x="34" y="156" class="a" font-size="11">live · measured</text>
<rect x="260" y="40" width="220" height="200" rx="4" class="box"/><text x="274" y="66" class="d" font-size="14">2 · Expert repo</text>
<text x="274" y="90" class="m" font-size="11">the work crosses into another</text><text x="274" y="106" class="m" font-size="11">repo's API: read its code at</text><text x="274" y="122" class="m" font-size="11">the pinned version</text>
<text x="274" y="156" class="m" font-size="11">built · unproven</text>
<rect x="500" y="40" width="220" height="200" rx="4" class="box"/><text x="514" y="66" class="d" font-size="14">3 · A live drone</text>
<text x="514" y="90" class="m" font-size="11">someone's drone works in that</text><text x="514" y="106" class="m" font-size="11">repo right now (borg top):</text><text x="514" y="122" class="m" font-size="11">ask it over the mesh; it</text><text x="514" y="138" class="m" font-size="11">answers at its next hook</text>
<text x="514" y="172" class="h" font-size="11">not built</text>
<rect x="740" y="40" width="240" height="200" rx="4" class="box-h"/><text x="754" y="66" class="d" font-size="14">4 · An engineer (the queen)</text>
<text x="754" y="90" class="m" font-size="11">an open decision no lesson</text><text x="754" y="106" class="m" font-size="11">answers waits in a queue; the</text><text x="754" y="122" class="m" font-size="11">drone asks it at the next</text><text x="754" y="138" class="m" font-size="11">session's first prompt</text>
<text x="754" y="172" class="h" font-size="11">queue live · turn-end asking off</text>
<line x1="240" y1="140" x2="260" y2="140" class="ln-a"/><line x1="480" y1="140" x2="500" y2="140" class="ln-a"/><line x1="720" y1="140" x2="740" y2="140" class="ln-a"/>
<text x="500" y="276" text-anchor="middle" class="m" font-size="11">nothing waits: answers arrive as free context in a later turn, and every answer becomes a lesson</text>""")

svg("team.svg", 1000, 412, "Machines linked peer to peer: a member's laptop, an offline member whose lessons keep working, a new member joining, an optional always-on peer", """
<defs><marker id="ar2" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0 0 L10 5 L0 10 z" class="f-a"/></marker></defs>
<circle cx="500" cy="190" r="70" class="box-a"/>
<text x="500" y="186" text-anchor="middle" class="d" font-size="18">shared lessons</text>
<text x="500" y="206" text-anchor="middle" class="m" font-size="11">on every machine</text>
<rect x="40" y="60" width="230" height="120" rx="5" class="box"/>
<text x="56" y="86" class="d" font-size="15">DevA · laptop</text>
<text x="56" y="106" class="m" font-size="11">3 drones, 2 repos</text>
<circle cx="74" cy="140" r="11" class="f-a"/><circle cx="110" cy="140" r="11" class="f-a"/><circle cx="146" cy="140" r="11" class="f-a"/>
<rect x="730" y="60" width="230" height="120" rx="5" class="box"/>
<text x="746" y="86" class="d" font-size="15">DevB · on vacation</text>
<text x="746" y="106" class="m" font-size="11">offline; their lessons keep</text>
<text x="746" y="122" class="m" font-size="11">steering everyone's drones</text>
<circle cx="764" cy="152" r="11" class="f-m"/><circle cx="800" cy="152" r="11" class="f-m"/>
<rect x="40" y="250" width="230" height="120" rx="5" class="box-h"/>
<text x="56" y="276" class="d" font-size="15">DevC · joins today</text>
<text x="56" y="296" class="m" font-size="11">joins with an invite ticket;</text>
<text x="56" y="312" class="m" font-size="11">gets every lesson in seconds;</text>
<text x="56" y="328" class="m" font-size="11">trust earned by outcomes</text>
<circle cx="74" cy="352" r="9" class="f-h"/>
<rect x="730" y="250" width="230" height="120" rx="5" class="box"/>
<text x="746" y="276" class="d" font-size="15">CI box · optional</text>
<text x="746" y="296" class="m" font-size="11">always-on peer for storage,</text>
<text x="746" y="312" class="m" font-size="11">never a coordinator</text>
<path class="ln-a dash" d="M270 120 L432 168"/>
<path class="ln dash" d="M730 120 L568 168"/>
<path class="ln-a" d="M270 310 L436 222" marker-end="url(#ar2)"/>
<path class="ln-a dash" d="M730 310 L566 220"/>
<text x="300" y="128" class="m" font-size="10">encrypted peer mesh</text>
<text x="300" y="300" class="h" font-size="10.5">catch-up ∝ what changed</text>
<text x="500" y="396" text-anchor="middle" class="m" font-size="11">Reasoning travels as git notes when work lands, plus copies on two peers. Code stays in git.</text>""")

svg("mesh-sync.svg", 1000, 430, "One sync session between two peers in six steps: hello, membership, reconcile lessons, reasoning replicas, ask, live presence and steering", """
<defs><marker id="ar4" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0 0 L10 5 L0 10 z" class="f-a"/></marker></defs>
<rect x="16" y="16" width="200" height="398" rx="5" class="box"/>
<text x="116" y="44" text-anchor="middle" class="d" font-size="16">Peer A</text>
<text x="116" y="62" text-anchor="middle" class="m" font-size="10.5">ed25519 key = its address</text>
<rect x="784" y="16" width="200" height="398" rx="5" class="box"/>
<text x="884" y="44" text-anchor="middle" class="d" font-size="16">Peer B</text>
<text x="884" y="62" text-anchor="middle" class="m" font-size="10.5">same loop, no leader</text>
<text x="500" y="44" text-anchor="middle" class="m" font-size="11">one encrypted QUIC connection (iroh), keys checked in the handshake</text>
<g font-size="11.5">
<path class="ln-a" d="M220 92 L780 92" marker-end="url(#ar4)"/><text x="236" y="84" class="d" font-size="13">1 · Hello</text><text x="380" y="84" class="m">which collective; a different name is refused</text>
<path class="ln-a" d="M220 148 L780 148" marker-end="url(#ar4)"/><text x="236" y="140" class="d" font-size="13">2 · Membership first</text><text x="420" y="140" class="m">signed member list; revocations before data</text>
<path class="ln-a" d="M220 204 L780 204" marker-end="url(#ar4)"/><text x="236" y="196" class="d" font-size="13">3 · Reconcile lessons</text><text x="430" y="196" class="m">compare fingerprints, send only missing lines</text>
<path class="ln-a dash" d="M220 260 L780 260" marker-end="url(#ar4)"/><text x="236" y="252" class="d" font-size="13">4 · Reasoning replicas</text><text x="440" y="252" class="m">git-note excerpts to 2 chosen peers, not all</text>
<path class="ln-a dash" d="M780 316 L220 316" marker-end="url(#ar4)"/><text x="236" y="308" class="d" font-size="13">5 · Ask</text><text x="330" y="308" class="m">"why was commit 3f2a made?" any holder answers</text>
<path class="ln-a dash" d="M220 372 L780 372" marker-end="url(#ar4)"/><text x="236" y="364" class="d" font-size="13">6 · Live</text><text x="330" y="364" class="m">who runs which drone; signed steering; kept a day at most</text>
</g>
<text x="116" y="100" text-anchor="middle" class="m" font-size="10.5">lessons.jsonl</text>
<text x="116" y="116" text-anchor="middle" class="m" font-size="10.5">membership</text>
<text x="116" y="132" text-anchor="middle" class="m" font-size="10.5">reasoning excerpts</text>
<text x="116" y="148" text-anchor="middle" class="m" font-size="10.5">presence, directives</text>
<text x="116" y="380" text-anchor="middle" class="a" font-size="10.5">episodes stay here</text>
<text x="116" y="396" text-anchor="middle" class="a" font-size="10.5">private stays here</text>""")
print("\n".join(sorted(p.name for p in OUT.iterdir())))
