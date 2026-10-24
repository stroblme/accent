# Remote hosts

You need SSH access to a Linux host running on x86-64 or AArch64.
Accent uses your system SSH settings, including hosts in `~/.ssh/config`, and uploads its helper when you connect.
The Flatpak can connect to a host with the same processor architecture as its own build.

## Open a vault

Choose **Open Remote…** on the start screen or in the menu.
Enter the SSH host and the vault's path on that host, then choose **Connect**.
The host field accepts an SSH config alias, and `~` in the path means your home folder there.
You can also open a vault with `accent ssh://user@host/path/to/vault`.

Your files stay on the host, where [search](search.md), [Git](git.md), language servers, and notebook kernels run.
**New Terminal** opens a shell there too; see [Terminals](terminals.md) for local and other remote shells.
Deleting a file from a remote vault is permanent, so Accent asks before doing it.

## Forward a port

Open the **Ports** pane to make a service on either computer reachable from the other.
For example, set **Local** to `8080` and **Remote** to `8000`, then choose **Add**.
Now `localhost:8080` reaches port `8000` on the host.
Click the arrow between the fields to reverse the direction, or use **Stop Forwarding** on a row to remove it.
Accent restores active forwards after a reconnect.

## Reconnect

If an established connection drops, Accent retries and shows the next attempt in a banner.
Choose **Reconnect Now** if you want to retry sooner or need to enter a passphrase.
If the first connection fails, choose **Reconnect** after fixing the problem.

## Remove uploaded helpers

Accent stores uploaded binaries in `~/.local/share/accent/server/` on the host, named `accent-cli-<hash>`.
An upload removes unused older builds and retains those still used by running processes.

To remove all uploaded binaries:

1. Let remote terminal jobs finish, then close their terminal tabs. Closing a window alone leaves its shells running.
2. Close all Accent windows connected to that host, including those on other computers using the same SSH account.
3. From a separate local terminal, run the following, replacing `user@host` with your SSH destination or config alias:

   ```sh
   ssh user@host 'rm -f -- "$HOME"/.local/share/accent/server/accent-cli-*'
   ```

This removes the uploaded binaries, leaving vault files and indexes in place.
Accent uploads its helper again the next time you connect.
