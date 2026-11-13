# System software belongs to the booted generation; user profiles remain independent.
if [ -f /usr/lib/recab/immutable ]; then
    case ":$PATH:" in *:/usr/lib/recipe/profile/bin:*) ;; *) PATH="/usr/lib/recipe/profile/bin:$PATH" ;; esac
    recipe_user_bin=
elif [ "$(id -u)" -eq 0 ]; then
    recipe_user_bin=/var/lib/recipe/profile/bin
else
    recipe_user_bin="${XDG_DATA_HOME:-$HOME/.local/share}/recipe/profile/bin"
fi
if [ "$(id -u)" -ne 0 ]; then recipe_user_bin="${XDG_DATA_HOME:-$HOME/.local/share}/recipe/profile/bin"; fi
if [ -n "$recipe_user_bin" ]; then case ":$PATH:" in *:"$recipe_user_bin":*) ;; *) PATH="$recipe_user_bin:$PATH" ;; esac; fi
export PATH
unset recipe_user_bin
