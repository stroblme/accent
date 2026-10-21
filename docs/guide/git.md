# Git

When a [vault](vaults.md) contains a Git repository, the **Git** pane appears in the sidebar.
If it contains more than one, choose the repository at the top of the pane.
Accent uses Git on the computer holding the vault, including a [remote host](remote.md).

## Review and commit changes

The pane separates merge conflicts, staged changes and other changes.
Click a file to inspect its diff.
Use **Stage** or **Unstage** on a file, folder or section, then enter a message and choose **Commit**.
If nothing is staged, Commit includes changed tracked files but leaves new files out.
Stage a new file first if it belongs in the commit.

**Discard** restores tracked changes or removes new files, so Accent asks before doing it.
The [Files pane](vaults.md) also marks changed files.

## Sync and branches

Choose **Sync** to pull and then push the current branch.
If the branch has no upstream, the button offers **Publish** instead.
Use **Sync All** beside the repository picker to sync every repository in the vault.

Click the branch name to switch branches, create one or merge another local branch into it.
Git checks whether switching is safe and tells you if it refuses.

## History and conflicts

The lower part of the pane shows commit history.
Click a commit to see its files, then click a file to compare its versions.
Choose **Load More** to see older commits.

If a merge stops on a conflict, open the file under **Merge Conflicts**.
The merge view shows your current version, the result and the incoming version side by side.
Take either side or both for each block, or edit the result directly.
Then choose **Mark Resolved** to save and stage it, and commit when all conflicts are resolved.
The banner in the Git pane lets you abort a merge or rebase.

See [Keyboard shortcuts](keyboard.md) for Git and merge actions.
