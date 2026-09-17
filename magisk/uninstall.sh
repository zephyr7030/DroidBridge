#!/system/bin/sh
MODDIR=${0%/*}
for pid in $(pidof droidbridge-supervisor droidbridged 2>/dev/null); do
    exe=$(readlink "/proc/$pid/exe" 2>/dev/null)
    case "$exe" in
        "$MODDIR/bin/droidbridge-supervisor"|"$MODDIR/bin/droidbridged") kill "$pid" 2>/dev/null ;;
    esac
done
