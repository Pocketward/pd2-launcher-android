# Logs

The app has two log levels. **Basic** is always on. **Detailed** adds more while **Settings → Help → Detailed logging** is on. Detailed logging is off by default, and it stays on until you turn it off.

Basic logs are enough to tell us what kind of problem you have, and to set up your game the same way as yours. When we need more, we ask you to turn on Detailed logging, make the problem occur again, and save the logs again.

## What each level records

| What | Basic | Detailed |
|---|---|---|
| Your device: CPU, memory, GPU and its driver, screen, Android build, heat state | Yes | Yes |
| Every app setting, and PD2's settings files | Yes | Yes |
| Errors, and each wait of more than 1 s for a new picture | Yes | Yes |
| The kinds of controllers and input devices (never their names) | Yes | Yes |
| The game's frame rate and CPU use | Every 30 s | Every 10 s |
| Where each frame's time goes (the frame split) | Every 30 s | Every 10 s |
| A frame summary at the end of each game | Yes | Yes |
| The display's statistics | No | Every 5 s |
| Where the pointer's delay goes | No | Yes |
| Your controller's keys and the pointer's moves (never a keyboard's keys) | No | Yes |
| The length of each text copied between Android and the game (never the text) | Yes | Yes |
| D2GL's settings at each change | No | Yes |
| Each file that the save sync copies (a character's name shows as `<name>`) | No | Yes |
| Wine's and Box64's own logs, and the game's network events | No | Yes, from the next **Play** |
| Every line of the official PD2 launcher's run | No | Yes |
| The server status on the Play screen | No | Yes |

The logs contain no CD keys and no passwords. They can show your character names and network addresses.

Detailed logging uses a little more CPU time, and the logs fill up faster. Turn it off when you have sent your logs.

To send logs, see [How do I send logs?](FAQ.md#how-do-i-send-logs) in the FAQ.
