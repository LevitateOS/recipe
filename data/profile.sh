# System software belongs to the booted generation; user profiles remain independent.
# On a host without an immutable system, software installed for this computer
# lives in /var/lib/recipe and is on everyone's PATH; a user's own software wins.
if [ -f /usr/lib/recab/immutable ]; then
    case ":$PATH:" in *:/usr/lib/recipe/profile/bin:*) ;; *) PATH="/usr/lib/recipe/profile/bin:$PATH" ;; esac
    recipe_user_bin=
else
    case ":$PATH:" in *:/var/lib/recipe/profile/bin:*) ;; *) PATH="/var/lib/recipe/profile/bin:$PATH" ;; esac
    recipe_user_bin=
fi
if [ "$(id -u)" -ne 0 ]; then recipe_user_bin="${XDG_DATA_HOME:-$HOME/.local/share}/recipe/profile/bin"; fi
if [ -n "$recipe_user_bin" ]; then case ":$PATH:" in *:"$recipe_user_bin":*) ;; *) PATH="$recipe_user_bin:$PATH" ;; esac; fi
export PATH
unset recipe_user_bin
