"""Sample Python fixture for integration tests."""


class UserService:
    """A simple user service."""

    def __init__(self):
        self.users = {}

    def create_user(self, name: str) -> dict:
        """Create a new user."""
        user = {"name": name, "id": len(self.users)}
        self.users[name] = user
        return user

    def get_user(self, name: str) -> dict | None:
        """Get a user by name."""
        return self.users.get(name)

    def delete_user(self, name: str) -> bool:
        """Delete a user."""
        return self.users.pop(name, None) is not None

    def list_users(self) -> list[dict]:
        """List all users."""
        return list(self.users.values())


def main():
    """Entry point."""
    service = UserService()
    service.create_user("alice")
    service.create_user("bob")
    for user in service.list_users():
        print(user["name"])


if __name__ == "__main__":
    main()
