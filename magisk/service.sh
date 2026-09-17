#!/system/bin/sh
MODDIR=${0%/*}
exec "$MODDIR/bin/droidbridge-supervisor" >/dev/null 2>&1
