import React from "react";

interface ProfileProps {
  userId: string;
}

export function Profile({ userId }: ProfileProps): React.JSX.Element {
  const [bio, setBio] = React.useState<string>("");
  const containerRef = React.useRef<HTMLDivElement>(null);

  React.useEffect(() => {
    fetch(`/api/users/${userId}/profile`)
      .then((r) => r.json())
      .then((data: { bio?: string }) => setBio(data.bio ?? ""));
  }, [userId]);

  React.useEffect(() => {
    // Seeded defect: user-controlled content injected as HTML — stored XSS.
    if (containerRef.current) {
      containerRef.current.innerHTML = bio;
    }
  }, [bio]);

  return (
    <section>
      <h2>Profile</h2>
      <div ref={containerRef} />
    </section>
  );
}
