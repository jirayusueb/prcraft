# Trial protocol (5 days)

The trial checks one claim: a member can finish a day of review in PR Craft without being forced to the forge's web page. AI review is not part of the trial; leave it off.

## Window

One shared window of 5 consecutive working days for every member.

## Each day, record

- Items cleared: reviews finished and changes you handled in the app.
- Forced forge opens: every time you had to use "Open on GitHub/GitLab" because the app could not do the job, and why.
- App version: from the about page.

A day with no record counts as failed.

## Fill the tally from the app

Every "Open on GitHub/GitLab" is counted on your Mac with where it came from. At the end of the day run:

```sh
pr-craft-cli tally.get                          # today
pr-craft-cli tally.get '{"date":"2026-11-28"}'  # another day (YYYY-MM-DD)
```

Copy `bumps` into the table. `suggested` gives a short guess at the reason for each open; correct it where it is wrong. The count stays on your Mac, in `tally.json` in the data folder, for 60 days. Nothing is sent anywhere.

## Pass

The trial passes when a majority of the N members, and at least 3, have 0 forced forge opens on 4 of the 5 days.

## Template

| Date | Member | Items cleared | Forced forge opens | Reason for each | App version |
| ---- | ------ | ------------- | ------------------ | --------------- | ----------- |
|      |        |               |                    |                 |             |
