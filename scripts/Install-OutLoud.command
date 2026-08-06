#!/bin/bash
# Double-clickable installer for OutLoud.
#
# WHY this exists alongside install.sh: macOS 26 shows "Possible Malware, Paste
# Blocked" when a user pastes a curl-pipe-bash line into Terminal. The warning
# is correct in general — that is exactly how scam installs work — and it is
# unanswerable in particular: the person being asked to click through a malware
# warning has no way to tell a gift from an attack, and should not be trained
# to dismiss that dialog.
#
# A file the user double-clicks never goes near the clipboard, so the paste
# guard never fires. Terminal opens it, runs it, and shows the output.
#
# The trade is a Gatekeeper prompt instead, but a much milder one: an unsigned
# .command produces the "unidentified developer" dialog, which right-click >
# Open does bypass, unlike the quarantine "damaged" error that has no override.

set -euo pipefail

# The window Terminal opens is small and the default is a wall of text, so this
# reads as a friendly note rather than a build log.
clear
cat <<'BANNER'

    OutLoud

    Hold a key, talk, and your words appear.

BANNER

# Run the real installer. Fetched rather than embedded so a fix to install.sh
# reaches everyone who still has this file, instead of freezing whatever was
# current the day it was sent.
URL="https://raw.githubusercontent.com/blarer/outloud/refs/heads/overlay/cat-mascot/scripts/install.sh"

if ! /usr/bin/curl -fsSL "$URL" -o /tmp/outloud-install.sh; then
    echo "  Could not reach the internet. Check the connection and try again."
    echo
    echo "  Press any key to close."
    read -r -n 1 -s
    exit 1
fi

/bin/bash /tmp/outloud-install.sh
rm -f /tmp/outloud-install.sh

echo
echo "  Press any key to close this window."
read -r -n 1 -s
