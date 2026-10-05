# Contributor License Agreement

Version 2, 2026-10-05. Nobody signed version 1, so no contribution was ever made under it.
<!-- source: ADR-0052 (UP-TAKE may be sold in stores, accepted 2026-10-05), which made version 1's
     "there is no paid version and no plan for one" untrue. "Nobody signed version 1" was checked on
     2026-10-05: no comment in VyLoneHQ/up-take carries the signing sentence, and every pull request so
     far is by the maintainer, his agent account or Dependabot. -->

Thank you for your interest in contributing to UP-TAKE. VyLone is the name under which David Supanz, an
individual in Austria, publishes and maintains UP-TAKE ("VyLone", "we", "us"). Section 6 covers
what happens if a company takes UP-TAKE over.
<!-- source: ADR-0005 (a CLA names the real legal name, not only the brand); the legal notice on
     vylone.com, which names the same person and says he operates as a private individual with no
     registered business, read 2026-10-05. "An individual" stays true once he registers as a sole
     trader, who is still a natural person; a company is section 6's case. -->

This Contributor License Agreement ("Agreement") sets out the terms on which you contribute, so that
contributors and users can be confident about where the code comes from and that it stays available.

By submitting a pull request, patch or other contribution to this repository, you agree to these terms
for that contribution and for every later one.

## 1. You keep your copyright

You retain all right, title and interest in and to your contributions. This Agreement does not
transfer ownership of your copyright to us.

## 2. You grant us a broad license

You grant VyLone a perpetual, worldwide, non-exclusive, royalty-free, irrevocable license to use,
reproduce, modify, prepare derivative works of, publicly display, publicly perform, sublicense and
distribute your contributions **and such derivative works**, under **any license terms VyLone chooses,
including proprietary and commercial licenses**, in addition to the project's current open-source
license (GPL-3.0-or-later). This includes distributing them as part of UP-TAKE through app stores such
as Steam, for a price or for free.

Plainly stated: UP-TAKE is free and complete. Its source code is free here under GPL-3.0-or-later, and
so is a ready-made download once releases exist. VyLone may also sell the same build in stores such as
Steam, under the same license, which the GPL allows on its own. A store copy buys convenience,
automatic updates and a way to support the work. It never buys extra features.

The license above reaches further than the GPL for one reason. It lets the project change its license
terms later if it ever has to, without tracking down every past contributor for permission. Without it,
merging your patch would freeze the project's ability to relicense anything, permanently, on one
unreachable person. It protects a fallback, not a product, and the open-source version stays
GPL-3.0-or-later either way.
<!-- source: ADR-0052 decisions 1, 2 and 5 (the same complete build may be sold in stores, the
     repository stays free, and GitHub also offers a free ready-made download); ADR-0010 decisions 1
     and 4 (free and complete, no feature gates); README.md "Nothing is installable yet" for "once
     releases exist"; GPL-3.0 section 4 ("You may charge any price or no price for each copy that you
     convey"). This paragraph said "there is no paid version and no plan for one" until 2026-10-05,
     and described a commercial "Pro" version until 2026-09-08. -->

## 3. You grant us a patent license

If you can license a patent claim that your contribution necessarily infringes, on its own or combined
with UP-TAKE as it stood when you contributed, you grant VyLone and everyone who receives UP-TAKE a
perpetual, worldwide, non-exclusive, royalty-free, irrevocable license under that claim to make, have
made, use, sell, offer for sale, import and otherwise transfer UP-TAKE with your contribution in it.

If anyone starts patent litigation, including a cross-claim or counterclaim, alleging that your
contribution or UP-TAKE infringes a patent, every patent license granted to that party under this
Agreement ends on the day the litigation is filed.
<!-- source: modelled on section 3 of the Apache Software Foundation's Individual Contributor License
     Agreement v2.2. GPL-3.0 section 11 already gives everyone who receives a GPL copy a patent license
     from each contributor. This section gives the same to the other license terms section 2 allows,
     which the GPL's own grant does not reach. -->

## 4. Your representations

By submitting a contribution, you represent that:

- You are legally entitled to grant the licenses above (for example, it is your original work, or you
  have permission from your employer if it is work for hire).
- You have said in the pull request which parts are not your original work, and named any third-party
  license, patent or other right you know applies to the contribution.
- Your contribution is provided "as is", without warranties of any kind.

If you wrote part of a contribution with an AI coding tool, please say so in the pull request. UP-TAKE
is built with one too, and the README says how.
<!-- source: README.md "How this is built"; WORKFLOW/PREFERENCES.md P-2 (AI involvement is disclosed).
     Asked because who holds the rights in AI-written code is unsettled, which bears on the first
     promise in this list. -->

## 5. No obligation

This Agreement does not oblige VyLone to use your contribution. It does not create an employment,
partnership or agency relationship.

## 6. If a company takes UP-TAKE over

VyLone may transfer this Agreement, and the licenses you grant in it, to a company or other legal
successor that takes over UP-TAKE, for example one its founder registers. The licenses continue
unchanged for that successor.
<!-- source: ADR-0005 "Revisit if" (once a legal entity sells, the copyright line moves to it); the
     company is not registered yet (the legal notice on vylone.com, 2026-10-05). -->

## 7. If this Agreement changes

Contributions you have already made stay under the version you accepted. If this Agreement changes, we
will ask you to accept the new version before we merge your next contribution.

## How to sign

Add a comment to your first pull request saying:

> I have read the UP-TAKE Contributor License Agreement and I accept it.

That comment is the record. It covers your later contributions to this repository as well, until this
Agreement changes (section 7). Until then, signing once is enough.

<!-- CORRECTED 2026-08-02. This section previously said signing happened "automatically" via a
     CLA-assistant bot. No such bot is installed: the repository has one workflow file, no CLA action,
     no CLA configuration and no webhooks, checked rather than assumed. A contributor following the
     old text would have opened a pull request, seen nothing, and had no way to sign a CLA that
     CONTRIBUTING.md makes mandatory. The manual comment above is the mechanism that works with no
     infrastructure. If a bot is installed later, this section changes back and the bot's record
     supersedes it. -->
