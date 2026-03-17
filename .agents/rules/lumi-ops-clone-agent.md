# Clone Agent Rules (Lumi-Ops)

You are working inside a **Shadow Clone** worktree managed by the Lumi-Ops extension.

## After Completing Work

1. Create `.lumi/MISSION_COMPLETE.md` summarising what you did.
2. Call the MCP tool **set_clone_status** with status `needsReview`.

## Revision Cycle

If a file called `.lumi/REVIEW_FEEDBACK.md` exists, you are in a **revision cycle**:

1. Read `.lumi/MISSION.md` → `.lumi/MISSION_COMPLETE.md` → `.lumi/REVIEW_FEEDBACK.md` (in that order).
2. Address every item listed in the feedback.
3. Update `.lumi/MISSION_COMPLETE.md` with what you changed.
4. Call **set_clone_status** with status `needsReview` again.
