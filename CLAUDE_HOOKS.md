# Claude Code Flash Integration

clitab can flash the tab when Claude Code needs user attention. This requires setting up Claude Code hooks.

## Setup

Add the following to your Claude Code settings file (`~/.claude/settings.json` or project `.claude/settings.json`):

```json
{
  "hooks": {
    "Notification": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "type": "command",
            "command": "printf '\\a'"
          }
        ]
      }
    ]
  }
}
```

## How it works

- **Notification hook**: Fires when Claude sends a notification or needs user input
- The `printf '\a'` command sends a BEL character to the terminal
- clitab detects the BEL character and flashes the tab

## Troubleshooting

If the hook doesn't work:

1. **Check if hooks are enabled**: Run `claude config list` to see current settings

2. **Test with a simple command**: Replace `printf '\a'` with `echo "hook triggered" >> /tmp/hook_test.log` to verify the hook fires

3. **Check Claude Code version**: Hooks require Claude Code v1.0.24 or later

4. **Try different matcher**: Change `"matcher": ".*"` to `"matcher": ""` (empty string)

## Alternative: Using OSC 9

For more precise control, you can use OSC 9 escape sequence:

```json
{
  "hooks": {
    "Notification": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "type": "command",
            "command": "printf '\\e]9;claude-done\\e\\\\'"
          }
        ]
      }
    ]
  }
}
```

## Note

The `Stop` event is a lifecycle event and may not support command hooks. Use `Notification` instead for reliable triggering.
