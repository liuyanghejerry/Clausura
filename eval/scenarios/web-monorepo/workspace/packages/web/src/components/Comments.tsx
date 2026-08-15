import React from "react";

interface CommentsProps {
  postId: string;
}

interface Comment {
  id: string;
  author: string;
  html: string;
}

export function Comments({ postId }: CommentsProps): React.JSX.Element {
  const [comments, setComments] = React.useState<Comment[]>([]);

  React.useEffect(() => {
    fetch(`/api/posts/${postId}/comments`)
      .then((r) => r.json())
      .then((data: Comment[]) => setComments(data));
  }, [postId]);

  return (
    <section>
      <h2>Comments</h2>
      <ul>
        {comments.map((c) => (
          <li key={c.id}>
            {/* Seeded defect: server HTML injected raw — stored XSS. */}
            <div dangerouslySetInnerHTML={{ __html: c.html }} />
          </li>
        ))}
      </ul>
    </section>
  );
}
