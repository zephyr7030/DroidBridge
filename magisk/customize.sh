SKIPUNZIP=0

set_perm "$MODPATH/service.sh" 0 0 0755
set_perm "$MODPATH/uninstall.sh" 0 0 0755
set_perm "$MODPATH/bin/droidbridge-supervisor" 0 0 0755
set_perm "$MODPATH/bin/droidbridged" 0 0 0755
set_perm "$MODPATH/bin/droidbridge-exec-guard" 0 0 0755
set_perm_recursive "$MODPATH/framework" 0 0 0755 0644
