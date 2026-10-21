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
