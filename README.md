# pcplayerpicker
Player Ranking App for Sports through Active Bayesian Learning

This app is built for my dad (Mark Catlin) as an upgraded version of the 1991 MS-DOS app he had designed called Player Picker. The primary idea was being able to understand soccer player's strengths by using 2x2 games in a series of small matches. That app was entirely text based and had to fit on a single floppy disk.

This version has a few upgrade, it has Bayesian based statistics for better understanding of the uncertainty of the rankings, and has an active learning mode, where player pairings for future rounds are aimed directly at figuring out uncertain rankings.

In order to be able to host this free for users, the infrastructure design is heavily focused on client side compute meaning whichever device runs the coach session does all the math on its own processor. The server really is then an optional add-on for allowing for players to view schedules on their own devices, and assistants to enter scores from their own devices. The Rust language was chosen for highly efficient computation, so that client devices can run this quickly and efficiently. Infrastructure is lightweight, with cloudflare workers handling requests and cloudflare D1 handling data, with the code designed to be able to be moved to a VM and local SQLite.
