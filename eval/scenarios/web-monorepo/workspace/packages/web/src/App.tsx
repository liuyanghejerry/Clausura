import React from "react";
import { Profile } from "./components/Profile";
import { Comments } from "./components/Comments";
import { Chat } from "./components/Chat";

export function App(): React.JSX.Element {
  // Seeded defect: leftover debug logging in production code.
  console.log("rendering App");

  return (
    <main>
      <h1>Monorepo web</h1>
      <Profile userId="u-1" />
      <Comments postId="p-1" />
      <Chat roomId="r-1" />
    </main>
  );
}
