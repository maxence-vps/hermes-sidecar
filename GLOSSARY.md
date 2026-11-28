# Glossary

**Project**: A named folder the user creates to group their Conversations and the Repositories they relate to. Projects only organise: the agent knows nothing about them. A Project's name is unique, ignoring case.
_Avoid_: workspace, folder

**Conversation**: An exchange with the agent, owned and stored by Hermes (a Hermes session). The sidecar never copies it; a Project only refers to it. A Conversation belongs to at most one Project, and may belong to none.
_Avoid_: session (Hermes's internal name), chat, thread

**Repository**: A git repository attached to a Project, known by its remote URL. A Project has any number of Repositories, each at most once; the same Repository may be attached to several Projects.
_Avoid_: repo

**Turn**: A single exchange where the user sends a message and Hermes runs its agent until completion.
_Avoid_: run, interaction
