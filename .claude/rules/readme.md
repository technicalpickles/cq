# README Design Decisions

These rules apply when editing README.md.

## Structure

The README has two parts with a clean break between them.

**Part 1: Screenplay montage.** Uses actual screenplay conventions (INT., TIGHT ON, CUT TO, TITLE CARD) to frame terminal output. Each scene is a direction line and a code block. No prose explanations between scenes.

**Part 2: Normal README.** Standard markdown for practical info: install, quick start, flags, views, use cases, license.

## The Montage Arc

The montage escalates through five scenes:

1. **INT. TERMINAL** sets the stage: you have session data you've never looked at
2. **First query** shows `cq tools` with bar chart output (the visual hook)
3. **TIGHT ON** zooms into something specific and personal (commands, errors, or similar)
4. **CUT TO: CLAUDE** pivots to Claude using cq, with a big SQL query (the "wall of SQL")
5. **The reveal** shows the query result, something genuinely surprising

The arc goes from "nice CLI" to "your AI agent can introspect on its own behavior." That's the actual pitch.

## Screenplay Direction Lines

Terse, evocative, cinematic. Think "A24 trailer" not "Marvel quip." Not jokey. The framing provides personality so the prose doesn't have to try hard.

## Voice

Medium intensity per the writing-voice skill. Contractions and natural phrasing, but no asides, editorializing, or fragment-sentence rhythm in the prose sections. The screenplay framing carries the personality.

## What Belongs Where

| Content | Location |
|---------|----------|
| Terminal output showcasing capabilities | Part 1 (montage) |
| Install instructions | Part 2 |
| Flag reference, view descriptions | Part 2 |
| Use case teasers + link to docs/use-cases.md | Part 2 |
| Agent consumption notes | Part 2 |

## Key Decisions

- The bar chart from `cq tools` is the visual hook, always Scene 2
- The "wall of SQL" in Scene 4 should be a real CTE query from docs/use-cases.md (skill activation gap preferred)
- The reveal should show numbers that are genuinely surprising, not just interesting
- Part 2 starts after a TITLE CARD with the one-liner description
- Keep Part 2 compact. Reference material, not persuasion.
