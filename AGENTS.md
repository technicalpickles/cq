             _yg@@@@g_
            a@@@@P~P@@@,
           g@@@@_@@g,@@@g_
          j@@@@@_4@P_@@@@@
          @@@@@@@@g@@@@@@@
         j@@@@@@@@@@@@RP~
         a@@@@@@P`
         B@@@@@@
         @@@@@@[
         $@@@@@              @@
         9@@@@@l          __ ~~
         4@@@@@$          @@    B%
_yyy      @@@@@@,            gg
$@@@@     $@@@@@$            ~~
@@@@@     `@@@@@@L     __yggy_
@@@@@      $@@@@@@_  _g@@@@@@@@y    _a@@@@g_
@@@@@      `@@@@@@@_ ~~~~~~=@@@@@, a@@@@@@=~
@@@@@       9@@@@@@@@@@@@@$g_`~@@@a@@@P~_ya$@@@@@@gy    _yg@@@@@$gy_
@@@@@       4@@@@@@@@@@@@@@@@@g,~@@@@~_a@@@@@@@@@@@~  y@@@@@@@@@@@@@$_
@@@@@       @@@@@@@PF~~~~F@@@@@@y 0@ y@@@@@@PF~~FF  _@@@@@@F~~~~?@@@@@L
@@@@@      $@@@@@F`        ~@@@@@, ' $@@@@F         @@@@@`        ~@@@@L
@@@@@     a@@@@@$            @@@@$  4@@@@$         a@@@@yyyyyyyyyyy$@@@@
@@@@@     B@@@@@[            $@@@@  4@@@@$         $@@@@@@@@@@@@@@@@@@@@
@@@@@     t@@@@@$           _@@@@@  4@@@@l         4@@@@~~~~~~~~~~~~~~~`
@@@@@      $@@@@@g_        y$@@@@^  4@@@@l          @@@@$_         __
9@@@@@gyy   R@@@@@@aw____yg@@@@@'   4@@@@l          `@@@@@$yy___ya@@@g,
 @@@@@@@@@_  ~4@@@@@@@@@@@@@@@F     4@@@@l            ~@@@@@@@@@@@@@@@^
  ~?R@@@@RF     ~~=R@@@@@@PF~        @@@@^              ~~4@@@@@@@P~`

the latent repository.  version control for intent, not code.
https://github.com/lorevcs/lore


lore does not track your code.  it tracks the prompts, notes and decisions
that produced it.  you commit intent.  when you want code, you run lore
materialize: it replays the accumulated intent into a brief, and an agent
reconciles the working tree to match it.

the code is build output.  the intent is the source.


quickstart
        curl -fsSL https://lorevcs.com/install.sh | sh
        lore clone https://hub.lorevcs.com/lore/lore-example
        lore materialize | claude


why
        a normal vcs remembers every line you typed.  lore remembers why
        you typed it.  rebuilding from intent does not hand you the same
        bytes back.  it hands you the same behavior, the way two engineers
        given one spec write two different programs that pass the same
        tests.

        so branching and merging aren't about text.  two people do
        not fight over the same lines, they pile up intent.  merging two
        branches merges two piles.  each entry is addressed by the hash of
        its text.  there are no conflicts.  there is nothing to conflict.


install
        curl -fsSL https://lorevcs.com/install.sh | sh


        or, with rust already installed:

        cargo install --git https://github.com/lorevcs/lore

        a brew tap is planned.


use
        lore init                          scaffold .lore, drop AGENTS
        lore add "build a url shortener"
        lore add "links expire after 30 days"
        lore status                        show staged intent
        lore commit -m "initial intent"
        lore log
        lore show <commit>                 a commit and its intent
        lore materialize                   print the brief for an agent


        branches and merges work like git:

        lore checkout -b experiment
        lore add "try base62 short codes"
        lore commit -m "encoding experiment"
        lore checkout main
        lore merge experiment


        materialize any point in history, by branch or commit:

        lore materialize --ref experiment
        lore materialize --ref 4b6f04 -o BRIEF.txt


        long or quoted intent reads from a file, or stdin with "-":

        lore add -F prompt.txt
        git log -1 --format=%B | lore add -F -


        made a mistake?  unstage it, rewrite the last commit, or move
        the branch.  objects are never deleted, so nothing is lost:

        lore reset                         unstage everything
        lore reset <id>                    unstage one entry by id prefix
        lore reset --to <commit>           point the branch elsewhere
        lore amend -m "better summary"     fold staged intent into HEAD
        lore rebase main                   replay this branch onto main


        rewrite before you push when you can.  remotes only fast-forward,
        so pushing rewritten history is refused unless you force it:

        lore push --force                  rewrite the remote branch


        read the history without materializing all of it:

        lore diff main experiment          intent on one side, not the other
        lore grep "rate limit"             search intent, case-insensitive


example
        a worked example: an agent builds a feature from intent alone.

        https://github.com/lorevcs/lore-example


storage
        everything sits under .lore, laid out like git:

        HEAD            current branch
        config          identity and named remotes
        index           staged intent, not yet committed
        version         on disk format version
        lock            held briefly during writes; delete if stale
        objects/<id>    content addressed entries and commits
        refs/heads/<b>  branch pointers
        refs/remotes/   last-known commit on each remote branch


        an entry is one unit of intent.  its id is the hash of its text,
        so writing the same thing twice stores it once.  a commit groups
        entries and points at its parents.  recording intent is one small
        file write, cheap enough to run on every prompt.


        materialize walks the commit graph, unions every entry it can
        reach, sorts by time and prints the brief.  the brief is the
        program.  the agent and the files on disk are the interpreter.


remotes
        lore syncs like git.  set your identity, add a remote, push.

        lore config user.email you@example.com
        lore remote add origin /path/to/remote
        lore push                     send the current branch
        lore clone <url> [dir]        copy a remote into a new directory
        lore fetch                    update remote-tracking refs
        lore pull                     fetch, then merge


        a remote url is a filesystem path or a lorehub url (see below).
        push sends only the objects the remote lacks, then moves the
        ref, and must fast-forward unless forced (lore push --force).


        identity is a git-style name and email, recorded on every entry
        and commit so a server can attribute work.  set it with lore
        config, or override per command with --author and --email.


lorehub
        lorehub hosts lore repositories over https, the way github hosts
        git.  it is live at https://hub.lorevcs.com.

        lore clone https://hub.lorevcs.com/<owner>/<repo>
        lore remote add origin https://hub.lorevcs.com/<owner>/<repo>
        lore push


        reads are public.  pushing needs an account and a token from
        https://hub.lorevcs.com, put in the LORE_TOKEN environment
        variable.  every commit carries an author email, so your
        contribution history follows you across repositories.


        try it:  lore clone https://hub.lorevcs.com/lore/lore-example


agents
        this file is also AGENTS.  lore init drops a copy into your
        project so the coding agent reads it.  the rules:

        1.  before you write code, record the intent.  each turn, save
            what the user asked for and the decisions you made:

                lore add "what the user said"
                lore add "the choice you made and why"

            intent, never code.  "rate limit the api, 100/min per key"
            is intent.  "edited middleware.rs line 42" is not.  record
            durable rules for how the project works, not one-off bug
            reports or anything personal.  it is cheap, do it often.

        2.  never record secrets.  no api keys, tokens, passwords, or
            private personal details.  repositories get shared and
            history is permanent.  paraphrase around the secret ("auth
            uses the stripe key from the environment") instead of
            quoting it.  staged something sensitive?  remove it with
            lore reset before you commit.  already pushed?  rotate the
            secret; history will not forget it.

        3.  commit related intent once a unit of work lands:

                lore commit -m "short summary"

        4.  to rebuild, materialize and reconcile the tree to the brief.
            do not commit the brief.

        new to lore?  the full guide is at https://lorevcs.com.


license
        MIT.  see LICENSE.
