#!/usr/bin/env bash
HOOK_DIR="$(git rev-parse --git-dir)/hooks"
for hook in .githooks/*; do
    ln -sf "../../$hook" "$HOOK_DIR/$(basename "$hook")"
    chmod +x "$hook"
done
echo "Hooks installed."