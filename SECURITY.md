# Security policy

DroidBridge gives AI agents real control over an Android device, so security reports are welcome
and taken seriously.

## Reporting a vulnerability

Please **do not open a public issue** for a vulnerability. Use GitHub's private reporting instead:
open the repository's **Security** tab and choose **Report a vulnerability**.

Include what you can of:

- the DroidBridge version (App and, if installed, the Magisk module);
- the device model, Android version, and root or Shizuku setup;
- the connection involved (ChatGPT tunnel or local MCP);
- steps to reproduce and the impact you observed.

You will get an acknowledgement as soon as the report is read. Please allow time for a fix and a
release before disclosing details publicly.

## Scope

In scope, for example:

- a way for another app on the device, or a network peer, to reach the local MCP endpoint or the
  Runtime without the user's token or consent;
- an agent obtaining more access than the user granted on the device;
- the Runtime API key or the local MCP token leaking to logs, exports, backups or other apps;
- the root helper, supervisor or execution guard running something other than their fixed
  commands;
- update verification accepting a package that is not signed with the release key.

Out of scope: what an agent does with access the user deliberately granted, and issues that
require an already compromised or rooted-by-an-attacker device.

## Supported versions

Only the latest release receives security fixes.
