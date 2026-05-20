======
Zoku
======

A minimalist terminal attach/detach tool.

Quick start
============

If you know `screen, tmux, zellij`, you already know how to use `zoku`. Create a new session named `demo`, run `bash` in it, and attach to it immediately:

.. code-block:: bash

    zoku new demo bash

This creates a Unix socket file named `demo` in current directory, which you can use to attach later:

.. code-block:: bash

    zoku attach demo

Upon attaching, `zoku` replays recent history in the session.

There is no escape key or super key to detach from a session. Just close your terminal window, or if you are inside a SSH connection, disconnect SSH with :kbd:`enter` and then :kbd:`shift + ~`.

Install
========

Install from crates.io:

.. code-block:: bash

    cargo install zoku

Install from source:

.. code-block:: bash

    git clone https://github.com/aiifabbf/zoku.git
    cargo install --path zoku

Why zoku?
==========

Consider a simple copy/paste scenario.

`screen`: Press :kbd:`ctrl + A` and then :kbd:`esc` to enter copy buffer, mouse drag to select, press :kbd:`ctrl + shift + C` to copy.

What is wrong?

+   Entering copy buffer through a 3-key combination is tedious.
+   Because scrolling is implemented by `screen` intercepting mouse scroll events, it can feel very laggy if behind an SSH connection.

`tmux`: If you do not enable mouse mode, it is the same as `screen` except you press :kbd:`ctrl + B` and then :kbd:`esc` to enter copy buffer. If you enable mouse mode, just mouse drag to select. Selected text goes into clipboard automatically.

What is wrong?

+   If not in mouse mode, it has all the problems `screen` has.
+   In mouse mode, `tmux` takes you to the very bottom after you release the mouse drag. Good luck to you realizing you just missed a line. Now you have to scroll all the way up again.
+   When you want to copy something from browser to `tmux`, you change focus from the browser window to the terminal emulator by clicking on the terminal emulator window. If you accidentally jitter your mouse slightly and fail to release the mouse at the precise position where you press the mouse, congratulations! `tmux` thinks that is a mouse drag and happily replaces your clipboard with garbage.

How does zoku fix the copy/paste problem?

+   `zoku` replays the recent history when you attach to a session, not showing you the content of another terminal emulator inside a terminal emulator. It does not intercept your mouse events. It sends them directly to the underlying programs.
+   Since this is a replay, all the content is already in your terminal emulator's native scroll buffer. You can use your terminal emulator's scroll bar, find, mouse drag to select and anything else. Use it as if the program had been started in this terminal in the first place. No data goes back and forth. Everything happens on your local computer. Smooth and fast.

Why no panel split? Your terminal emulator is very likely to have that functionality already.

Why no detach key? It complicates the implementation and brings no real benefit, as I find myself hardly detach `screen, tmux`. I usually just close the terminal window, or disconnect from SSH when I am done.

Origin of the name
==================

From 続（ぞく） in 継続（けいぞく）, meaning "continue, proceed".
